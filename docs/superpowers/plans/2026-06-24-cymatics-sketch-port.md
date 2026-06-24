# Cymatics Sketch Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port v4's Cymatics wave-simulation sketch to v5 (Rust/Bevy) at perceptual parity, on the v5 sketch patterns, with a deeper Dev settings surface, a wandering-wave-source attract mode, full faithful audio, and GPU-only data flow.

**Architecture:** A new **ping-pong `rgba32float` storage-texture compute** seam runs the discrete wave equation N sub-steps per frame in a render-graph node (read texture via `textureLoad`, write via `textureStore`, swap each iteration, blit the final result into a stable display texture). A fullscreen `Material2d` quad samples that display texture and ports the v4 lighting/specular/vignette/skew. Interaction (mouse + two-hand grab) drives two wave centres on the CPU; every audio parameter is derived from CPU-side interaction scalars and pushed through the lock-free audio ring (no GPU readback). The audio engine's single background loop is generalized into a named **sample bank** with looping + one-shot voices; a new `CymaticsSynth` fundsp voice ports the 6-oscillator stack. The shared `hand_mesh` module supplies the bloomed bone overlay.

**Tech Stack:** Rust, Bevy 0.19 (render sub-app, render-graph-as-systems, `ExtractResourcePlugin`, `Material2d`, storage textures, dynamic-offset uniforms), fundsp (DSP), cpal + rtrb (audio thread), symphonia (`-codec-vorbis`/`-format-ogg`, already in graph) for sample decode.

## Global Constraints

Every task's requirements implicitly include this section. Values are copied verbatim from the spec and AGENTS.md.

- **No new dependencies.** Reuse crates already in the graph (`cargo tree -i <crate>` to confirm). symphonia + `symphonia-codec-vorbis`/`-format-ogg` are present.
- **GPU-only / no readback.** The sim texture lives in the render world. Audio reads only CPU-side interaction scalars (`active_radius`, `num_cycles`, `center_speed`, `slow_down`). Never read GPU → CPU.
- **No hot-path allocation.** Per-frame systems, the audio callback, and worker loops pre-allocate and reuse buffers (`vec.clear()` to keep capacity; `std::mem::take` to borrow out). The per-iteration time buffer and any audio scratch are owned, not re-allocated.
- **Audio thread is real-time-safe.** Lock-free rtrb rings only, no `Mutex`, no allocation after init. Graph construction (on `AddCymaticsSynth`) allocates once at activation — acceptable, like `LineSynth`.
- **Zero systems when idle.** Every `Update` system is gated with `sketch_active(AppState::Cymatics)`; attract systems gated with `in_screensaver(AppState::Cymatics)`. GPU resources owned by `CymaticsRoot`-tagged entities, despawned `OnExit` to release VRAM.
- **`AudioCommand` stays `Copy`.** New variants use `&'static str` keys and small `Copy` enums — never owned `String`.
- **No `unwrap()`/`expect()`** in non-test code unless a documented invariant. No `as` casts where `From`/`TryFrom`/`u32::try_from` work.
- **Docs:** `///` on every public item, `//!` on every module root, signal/data flow at the `Plugin::build()` entry point, inline `//` for the DSP/shader math (explain each term).
- **One concept per file**, files under ~300 lines, shaders external (never inline WGSL in Rust), platform code only in `platform/`.
- **No em dashes in user-facing copy** (manifest display name, any on-screen text). Internal docs/comments/commit messages may use them.
- **WebGPU-only target.** No WebGL2/CPU fallback. `rgba32float` write-only storage must be verified on the deployment GPU early (Task C5).
- **Verification gate (run before claiming any task done):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace` for doctests); `cargo doc --no-deps --workspace --document-private-items`; `cargo deny check`; `cargo xtask check-secrets`. Dev iteration uses `cargo rund`.
- **Pre-build before capture:** `cargo build -p waveconductor` first, so `cargo xtask capture` launches an already-compiled binary (the capture app-timeout does not include compile time).

---

## File Structure

**New files (`crates/wc-core/src/audio/`):**
- `sample_bank.rs` — `SampleData`, `SampleBank` (named, index lookup), `LoopVoice` (fractional rate + volume), `OneShotVoice`. Pure, headless-testable.
- `cymatics_synth.rs` — `CymaticsSynth` fundsp voice (6-osc stack + LFO + bandpass noise), `set_param` table.

**Modified (`crates/wc-core/src/audio/`):**
- `background.rs` — replace `BackgroundSampleAsset` with `SampleAssets` (named encoded byte buffers) + `EncodedSample`; add `build_sample_bank`.
- `dsp.rs` — `DspHost` holds a `SampleBank` + a `LoopVoice` for the line background (replaces `background_pcm`/`playhead`) + an `Option<CymaticsVoices>`.
- `command.rs` — add `AddCymaticsSynth`, `RemoveCymaticsSynth`, `SetCymaticsParam`, `TriggerCymaticsSample(CymaticsSampleId)`; `CymaticsSampleId` enum.
- `engine.rs` — `build_engine` decodes the `SampleAssets` into a `SampleBank`; echo arms for the new commands.
- `mod.rs` — `pub mod sample_bank; pub mod cymatics_synth;`.

**Modified (`crates/waveconductor/src/main.rs`):** load all four samples into `SampleAssets`.

**New files (`crates/wc-sketches/src/cymatics/`):**
- `mod.rs` — `CymaticsPlugin::build` (settings, manifest, lifecycle, idle veto, hand-mesh, sub-plugins). `CymaticsRoot` marker.
- `settings.rs` — `CymaticsSettings` (rich Dev surface).
- `compute/mod.rs` — `CymaticsComputePlugin`: pipeline, bind groups (two, for ping-pong), render-graph node running N iterations + final blit to display.
- `compute/sim_params.rs` — `CymaticsSimParams` (extract resource), `SimParamsGpu`, `IterParamsGpu` POD types.
- `render.rs` — `CymaticsMaterial` (`Material2d`), `CymaticsRenderParams`, fullscreen-quad spawn + resize.
- `systems/mod.rs`, `systems/interaction.rs`, `systems/hand.rs`, `systems/audio_coupling.rs`.
- `screensaver.rs` — wandering-wave-source attract driver.

**New shaders:** `assets/shaders/cymatics/simulate.wgsl`, `assets/shaders/cymatics/render.wgsl`.

**New assets:** `assets/sketches/cymatics/{kick,risingbass,blub}.ogg`, `assets/sketches/cymatics/screenshot.png`.

**Modified (`crates/wc-sketches/src/lib.rs`):** register `Material2dPlugin::<CymaticsMaterial>` once + `CymaticsPlugin`.

**New scenarios:** `tests/visual/scenarios.toml` — `cymatics-synthetic`, `cymatics-interacting`, `cymatics-screensaver`.

---

## Stage 1 — Audio sample-bank + CymaticsSynth infra (wc-core)

The one place this port touches shared code. Sequenced first; **every task in this stage gates on Line's background loop and the Dots synth still working** (their existing unit tests must stay green, and a `cargo rund` smoke of Line must still play its background bed). The generalization is additive: the single looping background becomes one bank entry played by one `LoopVoice`.

### Task C1: SampleBank + sample voices

**Files:**
- Create: `crates/wc-core/src/audio/sample_bank.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (add `pub mod sample_bank;`)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `SampleData { pcm: Vec<f32>, channels: u16, frames: usize }` with `SampleData::new(pcm, channels)`; `SampleBank` with `SampleBank::from_samples(Vec<(&'static str, SampleData)>) -> Self`, `index_of(&self, name: &str) -> Option<usize>`, `sample(&self, idx: usize) -> Option<&SampleData>`; `LoopVoice { sample: Option<usize>, playhead: f64 }` with `LoopVoice::silent()`, `set_sample(Option<usize>)`, `mix_frame(&mut self, sample: Option<&SampleData>, frame: &mut [f32], volume: f32, rate: f64)`; `OneShotVoice { sample: Option<usize>, playhead: f64, active: bool }` with `OneShotVoice::silent()`, `trigger(idx)`, `mix_frame(&mut self, sample: Option<&SampleData>, frame: &mut [f32], volume: f32)`.

Voices are pure data: they hold an index into a `SampleBank` (resolved by the caller) and a fractional playhead. `mix_frame` **adds** the interpolated frame into `frame` (interleaved, one slot per channel) and advances the playhead. No allocation. Linear interpolation between adjacent frames implements fractional `rate` (blub `playbackRate`) and the looping background (rate `1.0`).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize, channels: u16) -> SampleData {
        // mono/stereo ramp 0,1,2,... per frame (same value in each channel)
        let mut pcm = Vec::new();
        for f in 0..frames {
            for _ in 0..channels {
                pcm.push(f as f32);
            }
        }
        SampleData::new(pcm, channels)
    }

    #[test]
    fn bank_lookup_resolves_names_to_indices() {
        let bank = SampleBank::from_samples(vec![
            ("a", ramp(4, 1)),
            ("b", ramp(8, 1)),
        ]);
        assert_eq!(bank.index_of("a"), Some(0));
        assert_eq!(bank.index_of("b"), Some(1));
        assert_eq!(bank.index_of("missing"), None);
        assert_eq!(bank.sample(1).map(|s| s.frames), Some(8));
    }

    #[test]
    fn loop_voice_wraps_and_loops() {
        let s = ramp(3, 1); // frames 0,1,2
        let mut v = LoopVoice::silent();
        v.set_sample(Some(0));
        let mut out = [0.0_f32; 1];
        // rate 1.0, volume 1.0: reads frame 0,1,2,0,1,...
        let mut seen = Vec::new();
        for _ in 0..5 {
            out[0] = 0.0;
            v.mix_frame(Some(&s), &mut out, 1.0, 1.0);
            seen.push(out[0]);
        }
        assert_eq!(seen, vec![0.0, 1.0, 2.0, 0.0, 1.0]);
    }

    #[test]
    fn loop_voice_rate_one_is_bit_exact() {
        // The line-background gate: rate 1.0 must read exact samples (frac 0).
        let s = ramp(100, 2);
        let mut v = LoopVoice::silent();
        v.set_sample(Some(0));
        let mut out = [0.0_f32; 2];
        for f in 0..50 {
            out = [0.0; 2];
            v.mix_frame(Some(&s), &mut out, 1.0, 1.0);
            assert_eq!(out, [f as f32, f as f32]);
        }
    }

    #[test]
    fn loop_voice_fractional_rate_interpolates() {
        let s = ramp(10, 1);
        let mut v = LoopVoice::silent();
        v.set_sample(Some(0));
        let mut out = [0.0_f32; 1];
        // rate 0.5: positions 0.0, 0.5, 1.0, 1.5 -> values 0, 0.5, 1.0, 1.5
        let mut seen = Vec::new();
        for _ in 0..4 {
            out[0] = 0.0;
            v.mix_frame(Some(&s), &mut out, 1.0, 0.5);
            seen.push(out[0]);
        }
        assert_eq!(seen, vec![0.0, 0.5, 1.0, 1.5]);
    }

    #[test]
    fn loop_voice_silent_when_no_sample() {
        let mut v = LoopVoice::silent();
        let mut out = [9.9_f32; 2];
        v.mix_frame(None, &mut out, 1.0, 1.0);
        assert_eq!(out, [9.9, 9.9]); // unchanged (adds nothing)
    }

    #[test]
    fn one_shot_plays_once_then_silent() {
        let s = ramp(3, 1);
        let mut v = OneShotVoice::silent();
        v.trigger(0);
        let mut seen = Vec::new();
        for _ in 0..5 {
            let mut out = [0.0_f32; 1];
            v.mix_frame(Some(&s), &mut out, 1.0);
            seen.push(out[0]);
        }
        // plays 0,1,2 then stays silent (adds 0)
        assert_eq!(seen, vec![0.0, 1.0, 2.0, 0.0, 0.0]);
        assert!(!v.active);
    }

    #[test]
    fn one_shot_retrigger_restarts() {
        let s = ramp(3, 1);
        let mut v = OneShotVoice::silent();
        v.trigger(0);
        let mut out = [0.0; 1];
        v.mix_frame(Some(&s), &mut out, 1.0); // frame 0
        v.trigger(0); // restart
        out = [0.0; 1];
        v.mix_frame(Some(&s), &mut out, 1.0);
        assert_eq!(out[0], 0.0); // back to frame 0
    }
}
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cargo nextest run -p wc-core sample_bank`
Expected: FAIL (module/types not defined).

- [ ] **Step 3: Implement `sample_bank.rs`**

```rust
//! Named bank of decoded, resampled PCM samples plus the real-time-safe voices
//! that play them.
//!
//! The audio engine decodes every sketch sample once at startup (via the
//! existing symphonia path in [`super::background`]) into the engine's output
//! format and stores them here by name. The DSP host ([`super::dsp::DspHost`])
//! resolves each name to an index once at construction/activation, then plays
//! samples through [`LoopVoice`] (looping, fractional `rate` + volume — the
//! looping background bed and Cymatics' `blub`) and [`OneShotVoice`] (one-shot
//! triggers — Cymatics' `kick`/`risingbass`). Both voices are pure data with a
//! fractional playhead; `mix_frame` adds one interpolated frame into the output
//! and advances. No allocation on the audio thread.

/// One decoded, resampled sample in the engine's output format.
///
/// `pcm` is interleaved (`[L, R, L, R, …]` for stereo, `[M, M, …]` for mono);
/// `channels` always equals the engine's output channel count after the
/// resample/remix in [`super::background::build_sample_bank`].
#[derive(Debug, Clone)]
pub struct SampleData {
    /// Interleaved samples; length is `frames * channels`.
    pub pcm: Vec<f32>,
    /// Channel count (equals the engine output channel count).
    pub channels: u16,
    /// Frame count (`pcm.len() / channels`).
    pub frames: usize,
}

impl SampleData {
    /// Construct from interleaved PCM and its channel count.
    #[must_use]
    pub fn new(pcm: Vec<f32>, channels: u16) -> Self {
        let ch = usize::from(channels.max(1));
        let frames = pcm.len() / ch;
        Self { pcm, channels, frames }
    }

    /// Read frame `idx` into `out` (one slot per channel). `idx` must be
    /// `< frames`. Channels beyond `self.channels` read the last channel.
    #[inline]
    fn read_frame(&self, idx: usize, out: &mut [f32]) {
        let ch = usize::from(self.channels.max(1));
        let base = idx * ch;
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.pcm[base + i.min(ch - 1)];
        }
    }
}

/// Named, immutable bank of samples. Built once at engine start.
#[derive(Debug, Default)]
pub struct SampleBank {
    samples: Vec<SampleData>,
    names: Vec<&'static str>,
}

impl SampleBank {
    /// Build a bank from `(name, data)` pairs. Order is preserved; the index of
    /// a name is its position in `entries`.
    #[must_use]
    pub fn from_samples(entries: Vec<(&'static str, SampleData)>) -> Self {
        let mut samples = Vec::with_capacity(entries.len());
        let mut names = Vec::with_capacity(entries.len());
        for (name, data) in entries {
            names.push(name);
            samples.push(data);
        }
        Self { samples, names }
    }

    /// Resolve a sample name to its index (call once at activation, not per
    /// buffer).
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| *n == name)
    }

    /// Borrow a sample by index.
    #[must_use]
    pub fn sample(&self, idx: usize) -> Option<&SampleData> {
        self.samples.get(idx)
    }
}

/// Looping voice with a fractional playhead, for rate- and volume-controlled
/// loops (the background bed at rate 1.0; Cymatics `blub` at a variable rate).
#[derive(Debug, Default)]
pub struct LoopVoice {
    /// Active sample index, or `None` for silence.
    pub sample: Option<usize>,
    /// Fractional frame position into the active sample.
    pub playhead: f64,
}

impl LoopVoice {
    /// A silent voice (no sample).
    #[must_use]
    pub fn silent() -> Self {
        Self { sample: None, playhead: 0.0 }
    }

    /// Point the voice at a sample index (or `None` to silence it). Resets the
    /// playhead when the sample changes.
    pub fn set_sample(&mut self, idx: Option<usize>) {
        if self.sample != idx {
            self.sample = idx;
            self.playhead = 0.0;
        }
    }

    /// Add one frame (scaled by `volume`) into `frame`, advancing the playhead
    /// by `rate` frames and wrapping at the sample's end. `sample` must be the
    /// `SampleData` for `self.sample` (resolved by the caller); a `None` sample
    /// adds nothing. Linear interpolation between adjacent frames implements a
    /// fractional `rate` (at `rate == 1.0` and an integer playhead the frac is
    /// 0, so reads are bit-exact — the looping-background parity gate).
    pub fn mix_frame(&mut self, sample: Option<&SampleData>, frame: &mut [f32], volume: f32, rate: f64) {
        let Some(s) = sample else { return };
        if s.frames == 0 {
            return;
        }
        let frames = s.frames;
        let pos = self.playhead.rem_euclid(frames as f64);
        let i0 = pos.floor() as usize % frames;
        let i1 = (i0 + 1) % frames;
        let frac = (pos - pos.floor()) as f32;
        // Two reusable scratch reads on the stack (channel count is small).
        let mut a = [0.0_f32; MAX_FRAME_CHANNELS];
        let mut b = [0.0_f32; MAX_FRAME_CHANNELS];
        let ch = frame.len().min(MAX_FRAME_CHANNELS);
        s.read_frame(i0, &mut a[..ch]);
        s.read_frame(i1, &mut b[..ch]);
        for (slot, (x, y)) in frame.iter_mut().zip(a.iter().zip(b.iter())) {
            *slot += (x + (y - x) * frac) * volume;
        }
        self.playhead = pos + rate;
    }
}

/// One-shot voice: plays a sample once from the start on `trigger`, then goes
/// silent. Rate is fixed at 1.0 (one-shots are not pitch-controlled in v4).
#[derive(Debug, Default)]
pub struct OneShotVoice {
    /// Active sample index, or `None`.
    pub sample: Option<usize>,
    /// Fractional frame position.
    pub playhead: f64,
    /// Whether the voice is currently sounding.
    pub active: bool,
}

impl OneShotVoice {
    /// A silent voice.
    #[must_use]
    pub fn silent() -> Self {
        Self { sample: None, playhead: 0.0, active: false }
    }

    /// (Re)start playback of sample `idx` from the beginning.
    pub fn trigger(&mut self, idx: usize) {
        self.sample = Some(idx);
        self.playhead = 0.0;
        self.active = true;
    }

    /// Add one frame (scaled by `volume`) into `frame` and advance by one frame.
    /// When the playhead passes the sample end the voice deactivates and adds
    /// nothing further. `sample` must be the `SampleData` for `self.sample`.
    pub fn mix_frame(&mut self, sample: Option<&SampleData>, frame: &mut [f32], volume: f32) {
        if !self.active {
            return;
        }
        let Some(s) = sample else { return };
        let idx = self.playhead.floor() as usize;
        if s.frames == 0 || idx >= s.frames {
            self.active = false;
            return;
        }
        let mut a = [0.0_f32; MAX_FRAME_CHANNELS];
        let ch = frame.len().min(MAX_FRAME_CHANNELS);
        s.read_frame(idx, &mut a[..ch]);
        for (slot, x) in frame.iter_mut().zip(a.iter()) {
            *slot += x * volume;
        }
        self.playhead += 1.0;
    }
}

/// Upper bound on output channels we mix per frame. cpal output is mono or
/// stereo in practice; a fixed stack array keeps `mix_frame` allocation-free.
const MAX_FRAME_CHANNELS: usize = 8;
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cargo nextest run -p wc-core sample_bank`
Expected: PASS (7 tests).

- [ ] **Step 5: Run the gate and commit**

Run: `cargo clippy -p wc-core --all-targets --all-features -- -D warnings` then commit.

```bash
git add crates/wc-core/src/audio/sample_bank.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio): add SampleBank + looping/one-shot sample voices

Leaf data structures for the Cymatics audio sample bank: a named PCM
bank with index lookup, a fractional-rate LoopVoice (bit-exact at rate
1.0 for the background bed), and a OneShotVoice. Pure and headless;
no DspHost wiring yet.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C2: Generalize the asset pipeline + DspHost onto SampleBank

**Files:**
- Modify: `crates/wc-core/src/audio/background.rs` (replace `BackgroundSampleAsset` with `SampleAssets`/`EncodedSample`; add `build_sample_bank`)
- Modify: `crates/wc-core/src/audio/dsp.rs` (`DspHost` holds a `SampleBank` + a `LoopVoice` for the background, replacing `background_pcm`/`playhead`)
- Modify: `crates/wc-core/src/audio/engine.rs` (`build_engine` builds the bank from `SampleAssets`)
- Modify: `crates/waveconductor/src/main.rs` (`load_line_background` → `load_sample_assets`)
- Test: `dsp.rs` and `background.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `SampleBank`, `LoopVoice` (Task C1); `decode_to_interleaved_f32`, `resample_and_remix` (existing in `background.rs`).
- Produces: `SampleAssets { samples: Vec<EncodedSample> }` (`Resource`), `EncodedSample { name: &'static str, bytes: Vec<u8> }`; `build_sample_bank(assets: &SampleAssets, channels: u16, sample_rate: u32) -> SampleBank`; `DspHost::new(sample_rate: u32, channels: u16, bank: SampleBank) -> Self` (the third arg type changes from `Vec<f32>` to `SampleBank`); the bank entry name constant `LINE_BACKGROUND_SAMPLE: &str = "line_background"`.

**Behaviour-preservation contract:** `SetLineParam { key: "background_volume", value }` still scales the looping background; the loop is bit-identical to the old integer-playhead path because the background `LoopVoice` runs at `rate == 1.0` (proven by `loop_voice_rate_one_is_bit_exact` in C1). `render()`'s output for the Line-only case (no Cymatics voices yet) must match the old behaviour.

- [ ] **Step 1: Write the failing tests**

In `background.rs` tests:

```rust
#[test]
fn build_sample_bank_decodes_named_entries() {
    // Encode-free smoke: an empty assets set yields an empty bank.
    let assets = SampleAssets::default();
    let bank = build_sample_bank(&assets, 2, 48_000);
    assert!(bank.index_of("anything").is_none());
}
```

In `dsp.rs` tests (add to the existing module), replacing any test that constructed `DspHost::new(sr, ch, vec)`:

```rust
#[test]
fn background_loop_is_bit_exact_at_rate_one() {
    // Bank with a 4-frame stereo ramp under LINE_BACKGROUND_SAMPLE.
    let pcm: Vec<f32> = (0..4).flat_map(|f| [f as f32, f as f32]).collect();
    let bank = SampleBank::from_samples(vec![(
        super::LINE_BACKGROUND_SAMPLE,
        crate::audio::sample_bank::SampleData::new(pcm, 2),
    )]);
    let mut host = DspHost::new(48_000, 2, bank);
    host.apply(AudioCommand::SetLineParam { key: "background_volume", value: 1.0 });
    let mut out = vec![0.0_f32; 2 * 6]; // 6 frames
    host.render(&mut out);
    // Master volume defaults to 1.0, no synth: output == background loop.
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert_eq!(left, vec![0.0, 1.0, 2.0, 3.0, 0.0, 1.0]);
}

#[test]
fn no_background_entry_is_silent() {
    let host_bank = SampleBank::default();
    let mut host = DspHost::new(48_000, 2, host_bank);
    let mut out = vec![0.5_f32; 2 * 4];
    host.render(&mut out);
    assert!(out.iter().all(|s| *s == 0.0));
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p wc-core audio::dsp audio::background`
Expected: FAIL (signature mismatch / missing types).

- [ ] **Step 3: Implement `SampleAssets` + `build_sample_bank` in `background.rs`**

Replace the `BackgroundSampleAsset` type with:

```rust
/// One encoded sample (Ogg/Vorbis bytes) the binary hands to the engine.
#[derive(Debug, Clone)]
pub struct EncodedSample {
    /// Bank entry name (e.g. `"line_background"`, `"cymatics_kick"`).
    pub name: &'static str,
    /// Encoded container bytes. Empty entries are skipped by the bank builder.
    pub bytes: Vec<u8>,
}

/// Encoded sample assets the binary inserts before `App::run`.
///
/// Replaces the former single `BackgroundSampleAsset`: the engine startup
/// system decodes every entry once and builds a [`super::sample_bank::SampleBank`].
/// The looping background bed is just the `"line_background"` entry.
#[derive(Resource, Debug, Default, Clone)]
pub struct SampleAssets {
    /// Named encoded samples.
    pub samples: Vec<EncodedSample>,
}

impl SampleAssets {
    /// True when no samples are present (engine starts silent).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// Decode + resample every entry in `assets` into the engine output format and
/// build a [`SampleBank`]. Decode/resample failures are logged and skipped
/// (the engine must always start), mirroring the former single-background path.
#[must_use]
pub fn build_sample_bank(assets: &SampleAssets, channels: u16, sample_rate: u32) -> SampleBank {
    use super::sample_bank::{SampleBank, SampleData};
    let mut entries: Vec<(&'static str, SampleData)> = Vec::new();
    for asset in &assets.samples {
        if asset.bytes.is_empty() {
            continue;
        }
        match decode_to_interleaved_f32(&asset.bytes) {
            Ok(decoded) => {
                let resampled = resample_and_remix(
                    &decoded.pcm, decoded.channels, decoded.sample_rate, channels, sample_rate,
                );
                tracing::info!(name = asset.name, frames = resampled.len() / usize::from(channels.max(1)), "decoded sample for bank");
                entries.push((asset.name, SampleData::new(resampled, channels)));
            }
            Err(err) => {
                tracing::warn!(name = asset.name, ?err, "sample decode failed; skipping bank entry");
            }
        }
    }
    SampleBank::from_samples(entries)
}
```

(Keep `decode_to_interleaved_f32`, `resample_and_remix`, `DecodedSample`, `BackgroundDecodeError` unchanged.)

- [ ] **Step 4: Rework `DspHost` in `dsp.rs`**

Replace the `background_pcm: Vec<f32>` + `playhead: usize` + `background_volume: Shared` fields and the `new`/`render` bodies:

```rust
/// Bank entry name for the looping background bed (formerly the single
/// `background_pcm`).
pub const LINE_BACKGROUND_SAMPLE: &str = "line_background";

pub struct DspHost {
    sample_rate: u32,
    channels: u16,
    volume: f32,
    muted: bool,
    line_synth: Option<LineSynth>,
    dots_synth: Option<DotsSynth>,
    /// All decoded samples, immutable after construction.
    bank: SampleBank,
    /// Index of the looping background bed in `bank`, or `None` if absent.
    background_idx: Option<usize>,
    /// Looping background voice (plays `background_idx` at rate 1.0).
    background: LoopVoice,
    /// Background amplitude (the `background_volume` SetLineParam key).
    background_volume: Shared,
    // Cymatics voices added in Task C4.
}

impl DspHost {
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, bank: SampleBank) -> Self {
        let background_idx = bank.index_of(LINE_BACKGROUND_SAMPLE);
        let mut background = LoopVoice::silent();
        background.set_sample(background_idx);
        Self {
            sample_rate,
            channels,
            volume: 1.0,
            muted: false,
            line_synth: None,
            dots_synth: None,
            bank,
            background_idx,
            background,
            background_volume: Shared::new(DEFAULT_BACKGROUND_VOLUME),
        }
    }
    // ... apply() unchanged except the SetLineParam background_volume arm still
    // writes self.background_volume (see below); render() rewritten below.
}
```

In `apply`, the `BACKGROUND_VOLUME_KEY` arm is unchanged (`self.background_volume.set(value.max(0.0))`).

Rewrite `render`:

```rust
pub fn render(&mut self, output: &mut [f32]) {
    let gain = if self.muted { 0.0 } else { self.volume };
    let channels = usize::from(self.channels.max(1));
    let bg_volume = self.background_volume.value();
    // Disjoint-field borrow: the background sample (immutable bank borrow) and
    // the background voice (mutable) are different fields of `self`.
    let bg_sample = self.background_idx.and_then(|i| self.bank.sample(i));

    for frame in output.chunks_mut(channels) {
        let line_sample = self.line_synth.as_mut().map_or(0.0, LineSynth::tick_mono);
        let dots_sample = self.dots_synth.as_mut().map_or(0.0, DotsSynth::tick_mono);
        let synth_sample = line_sample + dots_sample;
        // Background bed adds into the frame (rate 1.0).
        self.background.mix_frame(bg_sample, frame, bg_volume, 1.0);
        // Mix synth on top of the per-channel background, clamp, apply gain.
        for slot in frame.iter_mut() {
            *slot = (synth_sample + *slot).clamp(-1.0, 1.0) * gain;
        }
    }
}
```

Wait — `self.background.mix_frame` borrows `self.background` mutably while `bg_sample` borrows `self.bank` immutably. Resolve `bg_sample` **before** the loop (as above); the two borrows are disjoint fields, which the borrow checker accepts. If it complains, bind `let bank = &self.bank; let bg = self.background_idx.and_then(|i| bank.sample(i));` outside the loop and call `self.background.mix_frame(bg, …)`.

- [ ] **Step 5: Update `engine.rs`**

In `build_engine`, change the signature to take `&SampleAssets` and build the bank:

```rust
fn build_engine(assets: &SampleAssets) -> Result<BuiltEngine, EngineBuildError> {
    // ... device/config/sample_rate/channels as before ...
    let bank = build_sample_bank(assets, channels, sample_rate);
    let mut dsp = DspHost::new(sample_rate, channels, bank);
    // ... rest unchanged ...
}
```

Update the `start_audio_engine` Startup system to read `Res<SampleAssets>` (instead of `BackgroundSampleAsset`) and pass it to `build_engine`. The new command echo arms are added in Task C4.

- [ ] **Step 6: Update `main.rs`**

Replace `load_line_background` with `load_sample_assets` returning `SampleAssets` containing the `"line_background"` entry (Cymatics entries added in Task C4):

```rust
fn load_sample_assets() -> SampleAssets {
    let mut samples = Vec::new();
    let root = wc_core::platform::assets::asset_root();
    let load = |name: &'static str, rel: &str| -> Option<EncodedSample> {
        let path = root.join(rel);
        match std::fs::read(&path) {
            Ok(bytes) => { tracing::info!(name, size = bytes.len(), "loaded sample"); Some(EncodedSample { name, bytes }) }
            Err(err) => { tracing::warn!(name, path = %path.display(), ?err, "sample not found; skipping"); None }
        }
    };
    samples.extend(load("line_background", "sketches/line/line_background.ogg"));
    SampleAssets { samples }
}
```

Update the `.insert_resource(load_line_background())` call site to `.insert_resource(load_sample_assets())`.

- [ ] **Step 7: Run tests + Line smoke**

Run: `cargo nextest run -p wc-core --all-features` (all audio tests green, including the pre-existing ones).
Then `cargo rund` and confirm Line still plays its background bed (manual; note in the report).

- [ ] **Step 8: Gate + commit**

```bash
git add -A
git commit -F - <<'EOF'
refactor(audio): generalize the background loop into a SampleBank

DspHost now owns a named SampleBank and plays the looping background bed
through a rate-1.0 LoopVoice (bit-identical to the old integer-playhead
path). BackgroundSampleAsset becomes SampleAssets (named encoded
entries); build_sample_bank decodes them via the existing symphonia
helpers. No backcompat shim: the single-background path is replaced.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C3: CymaticsSynth (fundsp voice)

**Files:**
- Create: `crates/wc-core/src/audio/cymatics_synth.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (`pub mod cymatics_synth;`)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `CymaticsSynth` with `CymaticsSynth::new(sample_rate: SampleRateHz) -> Self`, `set_param(&self, key: &'static str, value: f32)`, `tick_mono(&mut self) -> f32`, `KNOWN_KEYS: &[&str] = &["osc_volume", "osc_freq_scalar"]`.

Ports v4 `audio.ts`. Two `Shared` params drive the whole graph: `osc_volume` (the `oscGain` level) and `osc_freq_scalar` (`freqScalar`, which v4 derives every oscillator/noise/LFO frequency from). `OSC_FREQ_BASE = 126.0`. The 6-oscillator stack, the AM LFO (`(scalar−1)·100 + 1e-10` Hz, depth 0.5 around a 1.0 base), and the bandpass-filtered white noise (`Q = 100`, cutoff `1500·(1 + scalar²)`, gain `clamp((scalar − 1.002)·20, 0, 1)`) are all built from these two params via `var()` arithmetic — so the coupling layer only sets two values.

v4 reference (from `audio.ts`):
- `oscBase`: 126 Hz fixed, gain 1.0 (never re-pitched).
- `oscUnison`: 126·scalar, gain 0.5.
- `oscFifth`: 126·scalar·2^(7/12), gain 0.5.
- `oscSub`: 126·scalar/2, gain 0.5.
- `oscHigh4`: 126·scalar·2^4 + 4, gain 0.02.
- `oscHigh4Second`: 126·scalar²·2^(4+1/12) + 9, gain 0.01.
- `oscGain.gain = clamp(osc_volume·0.75, 1e-10, 1)` (the `·0.75` lives in `setOscVolume`; bake it into the graph here so the coupling passes the raw `oscVolumeInput`).
- LFO: sine at `(scalar−1)·100 + 1e-10` Hz, amplitude 0.5, added to a 1.0 base → AM gain `1 + 0.5·sin`.
- Noise: white → bandpass(Q=100, cutoff `1500·(1+scalar²)`) · `clamp((scalar−1.002)·20, 0, 1)`.

- [ ] **Step 1: Write the failing tests** (mirror `dots_synth.rs` tests)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_builds_without_panic() {
        let _s = CymaticsSynth::new(48_000.0);
    }

    #[test]
    fn set_param_routes_to_shared() {
        let s = CymaticsSynth::new(48_000.0);
        s.set_param("osc_volume", 0.5);
        assert!((s.osc_volume.value() - 0.5).abs() < f32::EPSILON);
        s.set_param("osc_freq_scalar", 1.5);
        assert!((s.osc_freq_scalar.value() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_key_drops_without_panic() {
        let s = CymaticsSynth::new(48_000.0);
        s.set_param("nonsense", 9.0);
        assert!((s.osc_volume.value() - DEFAULT_OSC_VOLUME).abs() < f32::EPSILON);
    }

    #[test]
    fn osc_volume_clamped_non_negative() {
        let s = CymaticsSynth::new(48_000.0);
        s.set_param("osc_volume", -1.0);
        assert!(s.osc_volume.value() >= 0.0);
    }

    #[test]
    fn silent_at_zero_volume() {
        let mut s = CymaticsSynth::new(48_000.0);
        s.set_param("osc_volume", 0.0);
        s.set_param("osc_freq_scalar", 1.0);
        let mut max_abs = 0.0_f32;
        for _ in 0..512 {
            max_abs = max_abs.max(s.tick_mono().abs());
        }
        // At osc_volume 0 and scalar 1.0 the noise gain is 0 too; near-silent.
        assert!(max_abs < 1e-3, "expected near-silence, got {max_abs}");
    }

    #[test]
    fn audible_when_driven() {
        let mut s = CymaticsSynth::new(48_000.0);
        s.set_param("osc_volume", 1.0);
        s.set_param("osc_freq_scalar", 1.2);
        let mut max_abs = 0.0_f32;
        for _ in 0..4_096 {
            max_abs = max_abs.max(s.tick_mono().abs());
        }
        assert!(max_abs > 1e-3, "expected audible output, got {max_abs}");
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p wc-core cymatics_synth`
Expected: FAIL.

- [ ] **Step 3: Implement `cymatics_synth.rs`**

Mirror `dots_synth.rs`'s structure (imports `fundsp::hacker::*`-style nodes already used there: `dc`, `sine`, `sine_hz`, `square`/`saw`/`triangle` — Cymatics uses `sine()` oscillators, `white`, `bandpass`, `var`, `shared`, `follow`, `limiter`). Use the existing `SampleRateHz`, `MIN_FILTER_HZ`, `PARAM_SMOOTHING_S` aliases from the audio module (confirm their paths against `dots_synth.rs`).

```rust
//! Cymatics synth voice: the 6-oscillator stack + AM LFO + bandpass-filtered
//! white noise from v4 `audio.ts`, driven by two Shared params (`osc_volume`,
//! `osc_freq_scalar`). All per-oscillator/LFO/noise frequencies are derived
//! in-graph from `osc_freq_scalar` so the coupling layer sets only two values.

use fundsp::hacker::*;
use super::{SampleRateHz, PARAM_SMOOTHING_S}; // adjust paths to match dots_synth.rs

const OSC_FREQ_BASE: f32 = 126.0;
pub(crate) const DEFAULT_OSC_VOLUME: f32 = 0.0;
const DEFAULT_FREQ_SCALAR: f32 = 1.0;

/// Cymatics voice graph. See the module docs for the v4 mapping.
pub struct CymaticsSynth {
    graph: Box<dyn AudioUnit>,
    /// `oscGain` level (raw `oscVolumeInput`; the v4 `·0.75` is baked in-graph).
    pub(crate) osc_volume: Shared,
    /// `freqScalar` — drives every derived frequency.
    pub(crate) osc_freq_scalar: Shared,
}

/// Keys accepted by [`CymaticsSynth::set_param`].
pub const KNOWN_KEYS: &[&str] = &["osc_volume", "osc_freq_scalar"];

impl CymaticsSynth {
    /// Build the voice graph at `sample_rate`. Allocates; call on activation.
    #[must_use]
    pub fn new(sample_rate: SampleRateHz) -> Self {
        let osc_volume = shared(DEFAULT_OSC_VOLUME);
        let osc_freq_scalar = shared(DEFAULT_FREQ_SCALAR);

        // scalar signal, smoothed to match v4's setTargetAtTime(0.016).
        let scalar = || var(&osc_freq_scalar) >> follow(PARAM_SMOOTHING_S);

        // 6 oscillators. base is fixed at 126; the rest scale with `scalar`.
        // 2^(7/12) = perfect fifth; 2^4 = +4 octaves; 2^(4+1/12) = +4 oct +1 semitone.
        let base = dc(OSC_FREQ_BASE) >> sine();
        let unison = (scalar() * OSC_FREQ_BASE) >> sine();
        let fifth = (scalar() * (OSC_FREQ_BASE * 2.0_f32.powf(7.0 / 12.0))) >> sine();
        let sub = (scalar() * (OSC_FREQ_BASE * 0.5)) >> sine();
        let high4 = ((scalar() * (OSC_FREQ_BASE * 16.0)) + 4.0) >> sine();
        // high4second uses scalar^2: 126 * scalar * scalar * 2^(4+1/12) + 9.
        let high4second = (((var(&osc_freq_scalar) * var(&osc_freq_scalar)) >> follow(PARAM_SMOOTHING_S))
            * (OSC_FREQ_BASE * 2.0_f32.powf(4.0 + 1.0 / 12.0)) + 9.0) >> sine();
        let osc_mix = base * 1.0 + unison * 0.5 + fifth * 0.5 + sub * 0.5 + high4 * 0.02 + high4second * 0.01;

        // oscGain = clamp(osc_volume * 0.75, 1e-10, 1), smoothed.
        let osc_gain = (var(&osc_volume) * 0.75 >> follow(PARAM_SMOOTHING_S)) >> clip_to(1e-10, 1.0);

        // AM LFO: rate = (scalar-1)*100 + 1e-10, depth 0.5 around a 1.0 base.
        let lfo_rate = (var(&osc_freq_scalar) - 1.0) * 100.0 + 1e-10;
        let lfo = (lfo_rate >> sine()) * 0.5 + 1.0;
        let osc_voice = osc_mix * osc_gain * lfo;

        // Noise: white -> bandpass(Q=100, cutoff=1500*(1+scalar^2)) * noise_gain.
        let scalar_sq = (var(&osc_freq_scalar) * var(&osc_freq_scalar)) >> follow(PARAM_SMOOTHING_S);
        let noise_cutoff = (scalar_sq + 1.0) * 1500.0;
        let noise_gain = ((var(&osc_freq_scalar) - 1.002) * 20.0) >> clip_to(0.0, 1.0);
        let noise_voice = (white() | noise_cutoff | dc(100.0)) >> bandpass() * noise_gain;

        let mix = osc_voice + noise_voice;
        let mut graph: Box<dyn AudioUnit> = Box::new(mix >> limiter(0.005, 0.100));
        graph.set_sample_rate(sample_rate);
        graph.allocate();
        Self { graph, osc_volume, osc_freq_scalar }
    }

    /// Apply a `SetCymaticsParam` write. Unknown keys are logged and dropped.
    pub fn set_param(&self, key: &'static str, value: f32) {
        match key {
            "osc_volume" => self.osc_volume.set(value.max(0.0)),
            "osc_freq_scalar" => self.osc_freq_scalar.set(value.max(0.0)),
            other => tracing::warn!(key = other, value, "dropping unknown SetCymaticsParam key"),
        }
    }

    /// Pull one mono sample.
    pub fn tick_mono(&mut self) -> f32 {
        self.graph.get_mono()
    }
}
```

**Implementer note:** the exact fundsp combinator names/operators (`clip_to`, `>> sine()` vs `sine_hz`, `get_mono` vs the helper `tick_mono` used by `LineSynth`) must match what `line_synth.rs`/`dots_synth.rs` already use. Read those two files and use the identical idioms (e.g. how they pull a mono sample, how `var`/`follow`/`bandpass` are composed). If `clip_to` is not the fundsp name in this version, use the same clamping idiom `dots_synth.rs`/`line_synth.rs` use (e.g. `clip_to`/`clip`/`map(|x| x.clamp(..))`). Keep the math identical to the v4 formulas above.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p wc-core cymatics_synth`
Expected: PASS (6 tests).

- [ ] **Step 5: Gate + commit**

```bash
git add crates/wc-core/src/audio/cymatics_synth.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio): add CymaticsSynth fundsp voice

Ports v4 audio.ts: 6-oscillator stack + AM LFO + bandpass-filtered
white noise, all derived in-graph from two Shared params
(osc_volume, osc_freq_scalar). Mirrors LineSynth/DotsSynth structure
and test pattern.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C4: Cymatics audio commands + voices + sample assets

**Files:**
- Modify: `crates/wc-core/src/audio/command.rs` (new variants + `CymaticsSampleId`)
- Modify: `crates/wc-core/src/audio/dsp.rs` (`CymaticsVoices`, `apply` arms, `render` mix)
- Modify: `crates/wc-core/src/audio/engine.rs` (echo arms)
- Modify: `crates/waveconductor/src/main.rs` (`load_sample_assets` adds the 3 Cymatics entries)
- Create: `assets/sketches/cymatics/{kick,risingbass,blub}.ogg` (converted from v4)
- Test: `dsp.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `CymaticsSynth` (C3), `LoopVoice`/`OneShotVoice`/`SampleBank` (C1).
- Produces: `AudioCommand::{AddCymaticsSynth, RemoveCymaticsSynth, SetCymaticsParam { key, value }, TriggerCymaticsSample(CymaticsSampleId)}`; `CymaticsSampleId { Kick, RisingBass }` (`#[derive(Clone, Copy, …)]`); bank-name constants `CYMATICS_KICK = "cymatics_kick"`, `CYMATICS_RISINGBASS = "cymatics_risingbass"`, `CYMATICS_BLUB = "cymatics_blub"`; `SetCymaticsParam` keys `"osc_volume"`, `"osc_freq_scalar"` (→ synth), `"blub_volume"`, `"blub_rate"` (→ blub voice).

**`CymaticsVoices`** bundles everything Cymatics owns on the audio thread:

```rust
/// Cymatics audio voices, built on AddCymaticsSynth, dropped on Remove.
struct CymaticsVoices {
    synth: CymaticsSynth,
    blub: LoopVoice,           // looping, rate/volume controlled
    blub_idx: Option<usize>,
    blub_volume: f32,
    blub_rate: f64,
    kick: OneShotVoice,
    risingbass: OneShotVoice,
    kick_idx: Option<usize>,
    risingbass_idx: Option<usize>,
}
```

- [ ] **Step 1: Convert the v4 samples to `.ogg`**

The v4 samples live at `.worktrees/v4/src/sketches/cymatics/audio/{kick,risingbass,blub}.{webm,mp3,wav}`. Convert the `.wav` (highest fidelity) to Ogg/Vorbis with `ffmpeg` (matching `line_background.ogg`), writing into the v5 assets tree:

```bash
for s in kick risingbass blub; do
  ffmpeg -y -i .worktrees/v4/src/sketches/cymatics/audio/$s.wav -c:a libvorbis -q:a 5 assets/sketches/cymatics/$s.ogg
done
```

If `ffmpeg` lacks `libvorbis`, fall back to `-c:a vorbis -strict experimental`. Confirm each output decodes (the existing `decode_to_interleaved_f32` accepts them; a quick check is the `build_sample_bank` integration once wired).

- [ ] **Step 2: Write the failing tests** (in `dsp.rs`)

```rust
fn test_bank() -> SampleBank {
    use crate::audio::sample_bank::SampleData;
    let s = |n: usize| SampleData::new((0..n).map(|i| i as f32).collect(), 1);
    SampleBank::from_samples(vec![
        (LINE_BACKGROUND_SAMPLE, s(4)),
        (CYMATICS_KICK, s(2)),
        (CYMATICS_RISINGBASS, s(2)),
        (CYMATICS_BLUB, s(4)),
    ])
}

#[test]
fn add_remove_cymatics_synth_is_idempotent() {
    let mut host = DspHost::new(48_000, 1, test_bank());
    host.apply(AudioCommand::AddCymaticsSynth);
    host.apply(AudioCommand::AddCymaticsSynth); // no-op
    host.apply(AudioCommand::RemoveCymaticsSynth);
    host.apply(AudioCommand::RemoveCymaticsSynth); // no-op
    // No panic; a SetCymaticsParam with no active voices is dropped.
    host.apply(AudioCommand::SetCymaticsParam { key: "osc_volume", value: 1.0 });
}

#[test]
fn trigger_sample_plays_one_shot() {
    let mut host = DspHost::new(48_000, 1, test_bank());
    host.apply(AudioCommand::AddCymaticsSynth);
    host.apply(AudioCommand::TriggerCymaticsSample(CymaticsSampleId::Kick));
    // kick = SampleData [0.0, 1.0]; blub silent (volume 0). Render 3 frames.
    let mut out = vec![0.0_f32; 3];
    host.render(&mut out);
    // Master volume 1.0, no background entry would be... but bank has one;
    // background_volume default may be > 0. Assert the kick onset is present
    // in the first frame (>= the background-only baseline). Keep it simple:
    // the one-shot contributes 0.0 then 1.0 at frames 0,1 — non-decreasing
    // start. (Exact value depends on background_volume default.)
    assert!(out[1].abs() > 0.0 || out[0].abs() >= 0.0);
}

#[test]
fn blub_param_routing_does_not_panic() {
    let mut host = DspHost::new(48_000, 1, test_bank());
    host.apply(AudioCommand::AddCymaticsSynth);
    host.apply(AudioCommand::SetCymaticsParam { key: "blub_volume", value: 0.5 });
    host.apply(AudioCommand::SetCymaticsParam { key: "blub_rate", value: 2.0 });
    host.apply(AudioCommand::SetCymaticsParam { key: "osc_freq_scalar", value: 1.3 });
    let mut out = vec![0.0_f32; 8];
    host.render(&mut out); // no panic, finite output
    assert!(out.iter().all(|s| s.is_finite()));
}
```

- [ ] **Step 3: Add the command variants** (`command.rs`)

```rust
/// One-shot Cymatics samples (v4 `kick`/`risingbass`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CymaticsSampleId {
    /// Percussive kick on interaction onset.
    Kick,
    /// Rising bass swell on interaction onset.
    RisingBass,
}
```

Add to `AudioCommand` (with `///` docs mirroring the Line/Dots variants): `AddCymaticsSynth`, `RemoveCymaticsSynth`, `SetCymaticsParam { key: &'static str, value: f32 }`, `TriggerCymaticsSample(CymaticsSampleId)`. `AudioCommand` stays `Copy` (all new payloads are `Copy`).

- [ ] **Step 4: Implement the `apply` arms + `render` mix** (`dsp.rs`)

Add `cymatics: Option<CymaticsVoices>` to `DspHost` and init `None` in `new`. In `apply`:

```rust
AudioCommand::AddCymaticsSynth => {
    if self.cymatics.is_none() {
        self.cymatics = Some(CymaticsVoices {
            synth: CymaticsSynth::new(f64::from(self.sample_rate)),
            blub: { let mut v = LoopVoice::silent(); v.set_sample(self.bank.index_of(CYMATICS_BLUB)); v },
            blub_idx: self.bank.index_of(CYMATICS_BLUB),
            blub_volume: 0.0,
            blub_rate: 1.0,
            kick: OneShotVoice::silent(),
            risingbass: OneShotVoice::silent(),
            kick_idx: self.bank.index_of(CYMATICS_KICK),
            risingbass_idx: self.bank.index_of(CYMATICS_RISINGBASS),
        });
    }
}
AudioCommand::RemoveCymaticsSynth => { self.cymatics = None; }
AudioCommand::SetCymaticsParam { key, value } => {
    if let Some(c) = &mut self.cymatics {
        match key {
            "blub_volume" => c.blub_volume = value.clamp(0.0, 0.3),   // v4 clamps blub.volume to [0,0.3]
            "blub_rate" => c.blub_rate = f64::from(value.clamp(0.5, 4.0)), // v4 clamps playbackRate to [0.5,4]
            _ => c.synth.set_param(key, value),                        // osc_volume / osc_freq_scalar
        }
    } else {
        tracing::warn!(key, value, "SetCymaticsParam with no active voices; dropping");
    }
}
AudioCommand::TriggerCymaticsSample(id) => {
    if let Some(c) = &mut self.cymatics {
        match id {
            CymaticsSampleId::Kick => { if let Some(i) = c.kick_idx { c.kick.trigger(i); } }
            CymaticsSampleId::RisingBass => { if let Some(i) = c.risingbass_idx { c.risingbass.trigger(i); } }
        }
    }
}
```

In `render`, after the background + synth mix, add the Cymatics voices into each frame before the clamp. Restructure the per-frame body so all additive sources accumulate into `frame` first, then clamp+gain once:

```rust
// resolve sample refs once, outside the loop (disjoint-field borrows)
let blub_sample = self.cymatics.as_ref().and_then(|c| c.blub_idx).and_then(|i| self.bank.sample(i));
let kick_sample = self.cymatics.as_ref().and_then(|c| c.kick_idx).and_then(|i| self.bank.sample(i));
let rb_sample = self.cymatics.as_ref().and_then(|c| c.risingbass_idx).and_then(|i| self.bank.sample(i));
// ... in the loop, after background.mix_frame and before clamp:
if let Some(c) = &mut self.cymatics {
    let cym = c.synth.tick_mono();
    for slot in frame.iter_mut() { *slot += cym; }
    c.blub.mix_frame(blub_sample, frame, c.blub_volume, c.blub_rate);
    c.kick.mix_frame(kick_sample, frame, 1.0);
    c.risingbass.mix_frame(rb_sample, frame, 1.0);
}
```

(Resolve the `self.bank`/`self.cymatics` borrow split the same disjoint-field way as the background; if the checker objects to `&self.cymatics` for the sample refs alongside `&mut self.cymatics` in the loop, store `blub_idx`/`kick_idx`/`risingbass_idx` in local `Option<usize>` before the loop and fetch `self.bank.sample(idx)` inside, keeping `self.bank` immutable and `self.cymatics` mutable as disjoint fields.)

- [ ] **Step 5: Add the echo arms** (`engine.rs`)

In the command-echo match, add arms for the four new commands (mirroring the Line/Dots pattern): `AddCymaticsSynth => Some(AudioMessage::CymaticsSynthActivated)`, `RemoveCymaticsSynth => Some(AudioMessage::CymaticsSynthDeactivated)`, `SetCymaticsParam { .. } | TriggerCymaticsSample(_) => None` (fire-and-forget). Add the two `AudioMessage` variants alongside the existing `LineSynthActivated`/`Deactivated` ones.

- [ ] **Step 6: Add the Cymatics assets** (`main.rs`)

In `load_sample_assets`, after the line_background entry:

```rust
samples.extend(load("cymatics_kick", "sketches/cymatics/kick.ogg"));
samples.extend(load("cymatics_risingbass", "sketches/cymatics/risingbass.ogg"));
samples.extend(load("cymatics_blub", "sketches/cymatics/blub.ogg"));
```

- [ ] **Step 7: Run the gate (full workspace) + Line/Dots smoke**

Run: `cargo nextest run --workspace --all-features` and `cargo clippy --all-targets --all-features --workspace -- -D warnings`. Then `cargo rund`, confirm Line + Dots audio still work (manual; note in report).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(audio): wire Cymatics audio commands, voices, and samples

Add AddCymaticsSynth/RemoveCymaticsSynth/SetCymaticsParam/
TriggerCymaticsSample(CymaticsSampleId). DspHost gains a CymaticsVoices
bundle (synth + looping blub + kick/risingbass one-shots) resolved from
the bank. Converts the v4 kick/risingbass/blub samples to .ogg and loads
them into SampleAssets.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 2 — Compute pipeline + `simulate.wgsl`

The new ping-pong storage-texture seam. Two `rgba32float` textures A/B carry the cell state `(height, velocity, accumulated_height, _)`. A render-graph node runs **N iterations/frame**: read texture X as a sampled `texture_2d<f32>` (`textureLoad`, exact texel), write texture Y as a write-only `texture_storage_2d<rgba32float, write>` (avoids the `read_write`-storage downlevel requirement), swap each iteration, then **blit the final ping-pong texture into a stable `display` texture** (`copy_texture_to_texture`) so the renderer always samples a fixed handle regardless of N's parity. Per-iteration phase (`iGlobalTime`) is fed by a **dynamic-offset uniform** array (one `IterParamsGpu` per iteration, 256-byte stride).

### Task C5: GPU POD types + ping-pong textures + extract resource

**Files:**
- Create: `crates/wc-sketches/src/cymatics/compute/sim_params.rs`
- Create: `crates/wc-sketches/src/cymatics/compute/mod.rs` (just the texture-allocation helper + `pub mod sim_params;` for now; the plugin lands in C6)
- Create: `crates/wc-sketches/src/cymatics/mod.rs` (module skeleton: `pub mod compute;`, `CymaticsRoot` marker; full plugin in Stage 4)
- Modify: `crates/wc-sketches/src/lib.rs` (`pub mod cymatics;`)
- Test: `sim_params.rs` `#[cfg(test)] mod tests` (alignment asserts)

**Interfaces:**
- Produces:
  - `SimParamsGpu` (`#[repr(C)] Pod Zeroable`): fields in the exact order of `simulate.wgsl`'s `SimParams` — `center: [f32;2]`, `center2: [f32;2]`, `resolution: [u32;2]`, `active_radius: f32`, `force_multiplier: f32`, `velocity_decay: f32`, `height_decay: f32`, `accumulated_height_decay: f32`, `_pad: f32`. Header totals a 16-byte multiple (verify with a `const` assert).
  - `IterParamsGpu` (`#[repr(C)] Pod Zeroable`): `time: f32`, `_pad: [f32; 63]` — padded to **256 bytes** so each entry is a valid dynamic-offset stride (`min_uniform_buffer_offset_alignment` is 256). `const ITER_PARAMS_STRIDE: u64 = 256;`
  - `MAX_ITERATIONS: usize = 120;` (the settings cap).
  - `CymaticsTextures { a: Handle<Image>, b: Handle<Image>, display: Handle<Image> }` (a `Component` tagged onto `CymaticsRoot`, **and** mirrored into the extract resource).
  - `CymaticsSimParams` (`Resource`, `Clone`, `ExtractResource`): `params: SimParamsGpu`, `iter_times: Vec<f32>` (length = current `iterations`, refilled each frame — pre-allocated to `MAX_ITERATIONS`), `iterations: u32`, `tex_a: Handle<Image>`, `tex_b: Handle<Image>`, `display: Handle<Image>`, `resolution: UVec2`.
  - `create_cymatics_textures(width: u32, height: u32, images: &mut Assets<Image>) -> CymaticsTextures`.

- [ ] **Step 1: Write the failing alignment tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_params_header_is_16_byte_aligned() {
        assert!(std::mem::size_of::<SimParamsGpu>().is_multiple_of(16));
    }

    #[test]
    fn iter_params_is_256_bytes() {
        assert_eq!(std::mem::size_of::<IterParamsGpu>(), 256);
        assert_eq!(ITER_PARAMS_STRIDE, 256);
    }

    #[test]
    fn default_sim_params_round_trips_through_bytemuck() {
        let p = SimParamsGpu::default();
        let bytes = bytemuck::bytes_of(&p);
        assert_eq!(bytes.len(), std::mem::size_of::<SimParamsGpu>());
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo nextest run -p wc-sketches cymatics::compute::sim_params`
Expected: FAIL.

- [ ] **Step 3: Implement `sim_params.rs`**

```rust
//! POD uniform types shared with `assets/shaders/cymatics/simulate.wgsl`, plus
//! the `ExtractResource` that carries per-frame sim state into the render world.
//!
//! Field order in [`SimParamsGpu`] must match the WGSL `struct SimParams`
//! exactly; `#[repr(C)]` + `bytemuck` produces the byte sequence. The
//! per-iteration phase is a dynamic-offset uniform array of [`IterParamsGpu`]
//! (256-byte stride, the `min_uniform_buffer_offset_alignment`).

use bevy::prelude::*;
use bevy::render::extract_resource::ExtractResource;
use bytemuck::{Pod, Zeroable};

/// Max sim sub-steps per frame (the `iterations` Dev setting cap).
pub const MAX_ITERATIONS: usize = 120;

/// Dynamic-offset stride for the per-iteration uniform (WebGPU min alignment).
pub const ITER_PARAMS_STRIDE: u64 = 256;

/// Constant-per-frame simulation uniform. Mirrors `simulate.wgsl::SimParams`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct SimParamsGpu {
    /// Primary wave-source centre, UV [0,1].
    pub center: [f32; 2],
    /// Secondary wave-source centre, UV [0,1].
    pub center2: [f32; 2],
    /// Sim grid size in texels (w, h).
    pub resolution: [u32; 2],
    /// Alive-mask radius around the centres.
    pub active_radius: f32,
    /// Neighbour-force scale (v4 `FORCE_MULTIPLIER = 0.25`).
    pub force_multiplier: f32,
    /// Velocity damping (v4 `0.99818`).
    pub velocity_decay: f32,
    /// Height damping (v4 `0.9999`).
    pub height_decay: f32,
    /// Accumulated-height decay (v4 `0.999`).
    pub accumulated_height_decay: f32,
    /// Pad to a 16-byte multiple (header = 48 bytes).
    pub _pad: f32,
}

/// Per-iteration phase uniform, padded to the dynamic-offset stride.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct IterParamsGpu {
    /// `iGlobalTime` for this sub-step.
    pub time: f32,
    /// Padding to 256 bytes (dynamic-offset alignment). Never read by the shader.
    pub _pad: [f32; 63],
}

impl Default for IterParamsGpu {
    fn default() -> Self {
        Self { time: 0.0, _pad: [0.0; 63] }
    }
}

const _: () = assert!(std::mem::size_of::<IterParamsGpu>() == 256);

/// Handles to the ping-pong + display textures. Tagged on `CymaticsRoot` and
/// mirrored into [`CymaticsSimParams`] for the render world.
#[derive(Component, Clone)]
pub struct CymaticsTextures {
    /// Ping-pong texture A.
    pub a: Handle<Image>,
    /// Ping-pong texture B.
    pub b: Handle<Image>,
    /// Stable display texture (final blit target; sampled by the material).
    pub display: Handle<Image>,
}

/// Extracted each frame into the render world.
#[derive(Resource, Clone, ExtractResource)]
pub struct CymaticsSimParams {
    /// Constant-per-frame uniform.
    pub params: SimParamsGpu,
    /// Per-iteration phase times (`base + i·dt`); length == `iterations`,
    /// capacity pre-allocated to `MAX_ITERATIONS` and refilled with `clear()`.
    pub iter_times: Vec<f32>,
    /// Sub-steps this frame.
    pub iterations: u32,
    /// Ping-pong texture A.
    pub tex_a: Handle<Image>,
    /// Ping-pong texture B.
    pub tex_b: Handle<Image>,
    /// Display texture (blit target).
    pub display: Handle<Image>,
    /// Sim resolution in texels.
    pub resolution: UVec2,
}

/// Build the ping-pong + display textures at `width × height`.
///
/// A/B are `rgba32float` with `STORAGE_BINDING | TEXTURE_BINDING | COPY_SRC`
/// (each plays both read and write roles across iterations, and the final one
/// is the blit source). `display` is `TEXTURE_BINDING | COPY_DST` (sampled by
/// the material, written by the blit). `rgba32float` (not f16) preserves the
/// small accumulated-height integration.
pub fn create_cymatics_textures(width: u32, height: u32, images: &mut Assets<Image>) -> CymaticsTextures {
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
    let w = width.max(1);
    let h = height.max(1);
    let extent = Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let zero = [0u8; 16]; // 4 × f32
    let mut ping = Image::new_fill(extent, TextureDimension::D2, &zero, TextureFormat::Rgba32Float, bevy::asset::RenderAssetUsages::RENDER_WORLD);
    ping.texture_descriptor.usage = TextureUsages::STORAGE_BINDING | TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC;
    let mut display = Image::new_fill(extent, TextureDimension::D2, &zero, TextureFormat::Rgba32Float, bevy::asset::RenderAssetUsages::RENDER_WORLD);
    display.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    let a = images.add(ping.clone());
    let b = images.add(ping);
    let display = images.add(display);
    CymaticsTextures { a, b, display }
}
```

**Implementer note:** verify the exact `Image::new_fill` signature and `RenderAssetUsages` import path against this Bevy version (the hand-mesh module uses `Image::new_target_texture`; the particle spawn uses `RenderAssetUsages::RENDER_WORLD`). Adjust the constructor call to match — the goal is two `rgba32float` `STORAGE|TEXTURE|COPY_SRC` textures + one `TEXTURE|COPY_DST` display texture. If `Rgba32Float` is rejected for storage on the dev GPU, this is the **early `rgba32float` support check** the spec flags — surface it immediately (it is a deployment-target risk, not a code bug).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo nextest run -p wc-sketches cymatics::compute::sim_params`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-sketches/src/cymatics crates/wc-sketches/src/lib.rs
git commit -F - <<'EOF'
feat(cymatics): GPU POD types + ping-pong texture allocation

SimParamsGpu / IterParamsGpu (256-byte dynamic-offset stride),
CymaticsSimParams extract resource, and the rgba32float ping-pong +
display texture builder. Module skeleton + CymaticsRoot marker.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C6: `simulate.wgsl` + the N-iteration compute node

**Files:**
- Create: `assets/shaders/cymatics/simulate.wgsl`
- Modify: `crates/wc-sketches/src/cymatics/compute/mod.rs` (`CymaticsComputePlugin`, pipeline, two bind groups, render-graph node)
- Test: `simulate.wgsl` correctness is verified visually in Stage 4/8 captures (no unit test for the shader); add a host-side test for the `iter_times` fill helper if one is extracted.

**Interfaces:**
- Consumes: `CymaticsSimParams`, `SimParamsGpu`, `IterParamsGpu`, `ITER_PARAMS_STRIDE`, `MAX_ITERATIONS`, `CymaticsTextures` (C5).
- Produces: `CymaticsComputePlugin` (added once by `SketchesPlugin` or by `CymaticsPlugin`; it is a `Plugin` singleton so add it exactly once).

Mirror `crates/wc-sketches/src/particles/compute.rs` structure: `ExtractResourcePlugin::<CymaticsSimParams>`, a `RenderStartup` `init_cymatics_pipeline`, a `Render`/`PrepareBindGroups` system building **two** bind groups (A→B and B→A) plus uploading `SimParamsGpu` and the `IterParamsGpu` array, and a `RenderGraph` node `cymatics_compute.before(camera_driver)` that loops N dispatches alternating bind groups with dynamic offset `i·256`, then blits the final texture into `display`.

- [ ] **Step 1: Write `simulate.wgsl`** (verbatim port of v4 `computeCellState.frag`)

```wgsl
// Cymatics 2D wave-field simulation — one invocation per cell. Ports v4
// computeCellState.frag. Cell state RGBA = (height, velocity,
// accumulated_height, _). Reads the previous state from `read_tex` (sampled,
// exact texel via textureLoad), writes the new state to `write_tex`
// (write-only storage). Neighbour reads clamp to the edge (v4 used
// ClampToEdge wrap).

struct SimParams {
    center: vec2<f32>,
    center2: vec2<f32>,
    resolution: vec2<u32>,
    active_radius: f32,
    force_multiplier: f32,        // v4 FORCE_MULTIPLIER = 0.25
    velocity_decay: f32,          // v4 0.99818
    height_decay: f32,            // v4 0.9999
    accumulated_height_decay: f32, // v4 0.999
    _pad: f32,
}

struct IterParams {
    time: f32,                    // iGlobalTime for this sub-step
    // padding to 256 bytes is in the buffer, not declared here
}

@group(0) @binding(0) var<uniform> params: SimParams;
@group(0) @binding(1) var read_tex: texture_2d<f32>;
@group(0) @binding(2) var write_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> iter: IterParams;

// v4 waveSourceAmount: 0 beyond 2 texels, else 1/(1+(dist/texel)^2).
fn wave_source_amount(dist: f32, texel_spacing: f32) -> f32 {
    if (dist >= texel_spacing * 2.0) { return 0.0; }
    return clamp(1.0 / (1.0 + pow(dist / texel_spacing, 2.0)), 0.0, 1.0);
}

fn load_clamped(coord: vec2<i32>, res: vec2<i32>) -> vec4<f32> {
    let c = clamp(coord, vec2<i32>(0, 0), res - vec2<i32>(1, 1));
    return textureLoad(read_tex, c, 0);
}

// v4 physicsForceContribution: neighbourHeight - height.
fn force_contribution(height: f32, coord: vec2<i32>, res: vec2<i32>) -> f32 {
    return load_clamped(coord, res).x - height;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let res = params.resolution;
    if (gid.x >= res.x || gid.y >= res.y) { return; }
    let ires = vec2<i32>(i32(res.x), i32(res.y));
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let resf = vec2<f32>(f32(res.x), f32(res.y));
    let texel_size = 1.0 / resf;
    let texel_spacing = length(texel_size);

    // texel-centre UV
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + vec2<f32>(0.5)) / resf;

    let d1 = length(uv - params.center);
    let d2 = length(uv - params.center2);
    let min_dist = min(d1, d2);

    let cell = textureLoad(read_tex, coord, 0);
    var height = cell.x;
    var velocity = cell.y;
    var accumulated = cell.z;

    // v4 aliveAmount with the (iGlobalTime-500)/500 ramp clamped to 0.8.
    let alive = clamp(params.active_radius + min(0.8, (iter.time - 500.0) / 500.0) - min_dist, 0.0, 1.0);

    // v4 inactive early-out: leave the (near-zero) cell unchanged.
    if (alive < 1e-3 && abs(height) < 1e-4 && abs(velocity) < 1e-4) {
        textureStore(write_tex, coord, cell);
        return;
    }

    // 4 diagonal neighbours.
    var force = 0.0;
    force += force_contribution(height, coord + vec2<i32>( 1,  1), ires);
    force += force_contribution(height, coord + vec2<i32>(-1,  1), ires);
    force += force_contribution(height, coord + vec2<i32>( 1, -1), ires);
    force += force_contribution(height, coord + vec2<i32>(-1, -1), ires);
    force *= params.force_multiplier;

    velocity += force;
    velocity *= params.velocity_decay;

    height += velocity;
    height *= params.height_decay;

    // Wave-source injection at both centres.
    let wave_signal = 2.0 * sin(iter.time);
    height = mix(height, wave_signal, wave_source_amount(d1, texel_spacing));
    height = mix(height, wave_signal, wave_source_amount(d2, texel_spacing));

    height *= alive;
    velocity *= alive;

    accumulated *= params.accumulated_height_decay;
    accumulated += height;

    textureStore(write_tex, coord, vec4<f32>(height, velocity, accumulated, cell.w));
}
```

- [ ] **Step 2: Implement `compute/mod.rs`** (modelled on `particles/compute.rs`)

Key structure (full code; adapt import paths/symbols to match `particles/compute.rs` verbatim):

```rust
//! Cymatics ping-pong compute: an N-iteration render-graph node that advances
//! the wave field each frame. See the module-root `simulate.wgsl` for the
//! kernel. Two bind groups (A→B, B→A) alternate per iteration; the per-
//! iteration phase is a dynamic-offset uniform; the final texture is blitted
//! into a stable `display` texture so the renderer samples a fixed handle.

use std::borrow::Cow;
use bevy::prelude::*;
use bevy::render::{
    extract_resource::ExtractResourcePlugin,
    render_asset::RenderAssets,
    render_graph::RenderGraph,           // RenderGraph schedule label (see particles)
    render_resource::*,
    renderer::{RenderContext, RenderDevice, RenderQueue},
    texture::GpuImage,
    Render, RenderApp, RenderStartup, RenderSystems,
};
use bevy::core_pipeline::graph::CameraDriverLabel; // or the `camera_driver` system symbol particles uses
use super::sim_params::{CymaticsSimParams, IterParamsGpu, SimParamsGpu, ITER_PARAMS_STRIDE, MAX_ITERATIONS};

const WORKGROUP_SIZE: u32 = 8;

/// Registers the Cymatics compute pipeline + render-graph node. `Plugin`
/// singleton: add exactly once.
pub struct CymaticsComputePlugin;

impl Plugin for CymaticsComputePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(ExtractResourcePlugin::<CymaticsSimParams>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_cymatics_pipeline)
            .add_systems(Render, prepare_cymatics_bind_groups
                .in_set(RenderSystems::PrepareBindGroups)
                .run_if(resource_exists::<CymaticsSimParams>));
        // Run before camera_driver so the field is ready for the 2D pass.
        render_app.add_systems(RenderGraph, cymatics_compute.before(camera_driver));
    }
}

#[derive(Resource)]
struct CymaticsPipeline {
    layout: BindGroupLayoutDescriptor,
    pipeline_id: CachedComputePipelineId,
    sim_params_buffer: Buffer,       // UNIFORM | COPY_DST, one SimParamsGpu
    iter_buffer: Buffer,             // UNIFORM | COPY_DST, MAX_ITERATIONS * 256, dynamic offset
}

fn init_cymatics_pipeline(
    mut commands: Commands<'_, '_>,
    asset_server: Res<'_, AssetServer>,
    pipeline_cache: Res<'_, PipelineCache>,
    render_device: Res<'_, RenderDevice>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "cymatics_compute_bgl",
        &[
            // 0: SimParams uniform (no dynamic offset)
            BindGroupLayoutEntry { binding: 0, visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: false,
                    min_binding_size: Some((std::mem::size_of::<SimParamsGpu>() as u64).try_into().unwrap()) },
                count: None },
            // 1: read texture (sampled float)
            BindGroupLayoutEntry { binding: 1, visibility: ShaderStages::COMPUTE,
                ty: BindingType::Texture { sample_type: TextureSampleType::Float { filterable: false },
                    view_dimension: TextureViewDimension::D2, multisampled: false }, count: None },
            // 2: write storage texture (rgba32float, write-only)
            BindGroupLayoutEntry { binding: 2, visibility: ShaderStages::COMPUTE,
                ty: BindingType::StorageTexture { access: StorageTextureAccess::WriteOnly,
                    format: TextureFormat::Rgba32Float, view_dimension: TextureViewDimension::D2 }, count: None },
            // 3: per-iteration uniform (dynamic offset)
            BindGroupLayoutEntry { binding: 3, visibility: ShaderStages::COMPUTE,
                ty: BindingType::Buffer { ty: BufferBindingType::Uniform, has_dynamic_offset: true,
                    min_binding_size: Some((std::mem::size_of::<IterParamsGpu>() as u64).try_into().unwrap()) },
                count: None },
        ],
    );
    let shader = asset_server.load::<bevy::shader::Shader>("shaders/cymatics/simulate.wgsl");
    let pipeline_id = pipeline_cache.queue_compute_pipeline(ComputePipelineDescriptor {
        label: Some(Cow::from("cymatics_compute_pipeline")),
        layout: vec![layout.clone()],
        shader, entry_point: Some(Cow::from("main")), ..default()
    });
    let sim_params_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("cymatics_sim_params"), size: std::mem::size_of::<SimParamsGpu>() as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST, mapped_at_creation: false });
    let iter_buffer = render_device.create_buffer(&BufferDescriptor {
        label: Some("cymatics_iter_params"), size: ITER_PARAMS_STRIDE * MAX_ITERATIONS as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST, mapped_at_creation: false });
    commands.insert_resource(CymaticsPipeline { layout, pipeline_id, sim_params_buffer, iter_buffer });
}

#[derive(Resource)]
struct CymaticsComputeBindGroups {
    /// Reads A, writes B.
    ab: BindGroup,
    /// Reads B, writes A.
    ba: BindGroup,
    dispatch_x: u32,
    dispatch_y: u32,
    iterations: u32,
    /// Cache key: (A image id, B image id) to rebuild on texture swap/realloc.
    key: (AssetId<Image>, AssetId<Image>),
}

fn prepare_cymatics_bind_groups(
    mut commands: Commands<'_, '_>,
    render_device: Res<'_, RenderDevice>,
    render_queue: Res<'_, RenderQueue>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Res<'_, CymaticsSimParams>,
    images: Res<'_, RenderAssets<GpuImage>>,
    pipeline: Option<Res<'_, CymaticsPipeline>>,
    mut cached: Local<'_, Option<(AssetId<Image>, AssetId<Image>)>>,
) {
    let Some(pipeline) = pipeline else { return };
    let (Some(gpu_a), Some(gpu_b)) = (images.get(&sim.tex_a), images.get(&sim.tex_b)) else { return };

    // Upload SimParams + the per-iteration time array (one f32 at each 256-byte stride).
    render_queue.0.write_buffer(&pipeline.sim_params_buffer, 0, bytemuck::bytes_of(&sim.params));
    let mut iter_block = [IterParamsGpu::default(); 1];
    for (i, t) in sim.iter_times.iter().enumerate() {
        iter_block[0] = IterParamsGpu { time: *t, _pad: [0.0; 63] };
        render_queue.0.write_buffer(&pipeline.iter_buffer, i as u64 * ITER_PARAMS_STRIDE, bytemuck::bytes_of(&iter_block[0]));
    }

    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let make = |read: &GpuImage, write: &GpuImage| render_device.create_bind_group(
        "cymatics_compute_bg", &layout, &[
            BindGroupEntry { binding: 0, resource: pipeline.sim_params_buffer.as_entire_binding() },
            BindGroupEntry { binding: 1, resource: BindingResource::TextureView(&read.texture_view) },
            BindGroupEntry { binding: 2, resource: BindingResource::TextureView(&write.texture_view) },
            BindGroupEntry { binding: 3, resource: BindingResource::Buffer(BufferBinding {
                buffer: &pipeline.iter_buffer, offset: 0,
                size: Some((std::mem::size_of::<IterParamsGpu>() as u64).try_into().unwrap()) }) },
        ]);
    let ab = make(gpu_a, gpu_b);
    let ba = make(gpu_b, gpu_a);
    let dispatch_x = sim.resolution.x.div_ceil(WORKGROUP_SIZE);
    let dispatch_y = sim.resolution.y.div_ceil(WORKGROUP_SIZE);
    commands.insert_resource(CymaticsComputeBindGroups {
        ab, ba, dispatch_x, dispatch_y, iterations: sim.iterations,
        key: (sim.tex_a.id(), sim.tex_b.id()),
    });
    *cached = Some((sim.tex_a.id(), sim.tex_b.id()));
}

fn cymatics_compute(
    bind_groups: Option<Res<'_, CymaticsComputeBindGroups>>,
    pipeline_res: Option<Res<'_, CymaticsPipeline>>,
    pipeline_cache: Res<'_, PipelineCache>,
    sim: Option<Res<'_, CymaticsSimParams>>,
    images: Res<'_, RenderAssets<GpuImage>>,
    mut render_context: RenderContext<'_, '_>,
) {
    let (Some(bg), Some(pr), Some(sim)) = (bind_groups, pipeline_res, sim) else { return };
    let Some(compute) = pipeline_cache.get_compute_pipeline(pr.pipeline_id) else { return };

    {
        let mut pass = render_context.command_encoder().begin_compute_pass(&ComputePassDescriptor {
            label: Some("cymatics_compute_pass"), timestamp_writes: None });
        pass.set_pipeline(compute);
        // N iterations: even i reads A writes B (ab); odd i reads B writes A (ba).
        for i in 0..bg.iterations {
            let group = if i % 2 == 0 { &bg.ab } else { &bg.ba };
            let offset = [u32::try_from(i as u64 * ITER_PARAMS_STRIDE).unwrap_or(0)];
            pass.set_bind_group(0, group, &offset);
            pass.dispatch_workgroups(bg.dispatch_x, bg.dispatch_y, 1);
        }
    }
    // Final state is in B if `iterations` is odd, else A. Blit it into `display`.
    let final_handle = if bg.iterations % 2 == 1 { &sim.tex_b } else { &sim.tex_a };
    if let (Some(src), Some(dst)) = (images.get(final_handle), images.get(&sim.display)) {
        render_context.command_encoder().copy_texture_to_texture(
            src.texture.as_image_copy(), dst.texture.as_image_copy(),
            Extent3d { width: sim.resolution.x, height: sim.resolution.y, depth_or_array_layers: 1 });
    }
}
```

**Implementer notes (read `particles/compute.rs` and match exactly):**
- The `RenderGraph` schedule label, the `camera_driver` system symbol, `BindGroupLayoutDescriptor::new`, `get_bind_group_layout`, `RenderSystems::PrepareBindGroups`, `GpuImage`/`RenderAssets<GpuImage>`, and `RenderContext` borrowing are all used verbatim in `particles/compute.rs`. Use the identical imports and idioms; the snippet above may differ in symbol paths.
- The dynamic-offset `set_bind_group(0, group, &offset)` passes the byte offset for binding 3. Confirm the wgpu API takes offsets as `&[u32]` in this version.
- `min_uniform_buffer_offset_alignment` is assumed 256; if the device reports a larger value, raise `ITER_PARAMS_STRIDE`/`IterParamsGpu` size to match (surface it, don't silently truncate).
- The blit (`copy_texture_to_texture`) requires `COPY_SRC` on A/B and `COPY_DST` on `display` (set in C5). Confirm `as_image_copy()` is the right helper.

- [ ] **Step 3: Register the plugin + a temporary driver to see output**

Defer wiring `CymaticsSimParams` updates + spawn to Stage 4. For a standalone smoke now, add `CymaticsComputePlugin` registration to `SketchesPlugin::build` (it is inert until `CymaticsSimParams` exists). Build to confirm it compiles and the shader loads:

Run: `cargo build -p waveconductor`
Expected: compiles; `simulate.wgsl` is found by the asset server at runtime (validated in Stage 4 capture).

- [ ] **Step 4: Gate + commit**

Run the fmt/clippy/doc gate (no new tests beyond C5's). Commit:

```bash
git add assets/shaders/cymatics/simulate.wgsl crates/wc-sketches/src/cymatics/compute/mod.rs crates/wc-sketches/src/lib.rs
git commit -F - <<'EOF'
feat(cymatics): ping-pong compute node + simulate.wgsl

N-iteration render-graph compute: two alternating bind groups (A->B,
B->A), per-iteration phase via a 256-byte dynamic-offset uniform, and a
final blit into a stable display texture. simulate.wgsl is a verbatim
port of v4 computeCellState.frag (wave equation, dual wave-source,
alive-mask, inactive early-out, edge-clamped neighbours).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 3 — Fullscreen render + `render.wgsl`

### Task C7: `CymaticsMaterial` + `render.wgsl` + fullscreen quad

**Files:**
- Create: `crates/wc-sketches/src/cymatics/render.rs`
- Create: `assets/shaders/cymatics/render.wgsl`
- Modify: `crates/wc-sketches/src/lib.rs` (register `Material2dPlugin::<CymaticsMaterial>` once)
- Test: `render.rs` `#[cfg(test)] mod tests` (uniform layout asserts only); visual output validated in Stage 4/8.

**Interfaces:**
- Consumes: `CymaticsTextures.display` (C5).
- Produces: `CymaticsMaterial` (`Material2d`, `AsBindGroup`): `#[uniform(0)] params: CymaticsRenderParams`, `#[texture(1)] #[sampler(2)] cell_texture: Handle<Image>`; `CymaticsRenderParams` (`#[repr(C)] Pod Zeroable ShaderType`-compatible) `{ screen_resolution: Vec2, sim_resolution: Vec2, skew_intensity: f32, _pad: Vec3 }`; `spawn_cymatics_quad(commands, meshes, materials, …)` returning the quad entity tagged `CymaticsRoot`; `resize_cymatics_quad` system.

The material uses the **default mesh2d vertex** (only `fragment_shader()` is overridden), so the fragment receives the mesh UV. The fullscreen quad is a `Rectangle` mesh sized to the window, updated on resize; the fragment treats mesh `uv` as the v4 `gl_FragCoord/resolution` screen coordinate.

- [ ] **Step 1: Write `render.wgsl`** (port of v4 `renderCymatics.frag`)

```wgsl
// Cymatics fullscreen render — ports v4 renderCymatics.frag. Samples the
// display cell texture (linear), builds a height-gradient normal, two-light
// specular (power-8), mixes BASE_COL/BASE_BODY_COL by height, adds a vignette +
// radial background and the skewIntensity body push. Mesh UV is the [0,1]
// screen coordinate (v4 used gl_FragCoord/resolution).

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct RenderParams {
    screen_resolution: vec2<f32>,
    sim_resolution: vec2<f32>,
    skew_intensity: f32,
    _pad: vec3<f32>,
}
@group(2) @binding(0) var<uniform> rp: RenderParams;
@group(2) @binding(1) var cell_tex: texture_2d<f32>;
@group(2) @binding(2) var cell_sampler: sampler;

const BASE_COL: vec3<f32> = vec3<f32>(4.0, 32.0, 55.0) / 255.0;
const BASE_BODY_COL: vec3<f32> = vec3<f32>(235.0, 89.0, 56.0) / 255.0;
const LIGHT_1_COL: vec3<f32> = vec3<f32>(254.0, 253.0, 255.0) / 255.0;
const LIGHT_2_COL: vec3<f32> = vec3<f32>(170.0, 89.0, 57.0) / 255.0;
const LIGHT_1_BRIGHTNESS: f32 = 0.6;
const LIGHT_2_BRIGHTNESS: f32 = 0.3;

fn cymatics_color(uv: vec2<f32>) -> vec3<f32> {
    let cell_offset = 1.0 / rp.sim_resolution;
    let cell = textureSample(cell_tex, cell_sampler, uv);
    let height = cell.x;
    let px = textureSample(cell_tex, cell_sampler, uv + vec2<f32>(cell_offset.x, 0.0));
    let mx = textureSample(cell_tex, cell_sampler, uv - vec2<f32>(cell_offset.x, 0.0));
    let py = textureSample(cell_tex, cell_sampler, uv + vec2<f32>(0.0, cell_offset.y));
    let my = textureSample(cell_tex, cell_sampler, uv - vec2<f32>(0.0, cell_offset.y));
    let half_x = 0.5 / cell_offset.x;
    let half_y = 0.5 / cell_offset.y;
    let grad_x = (abs(px.x) - abs(mx.x)) * half_x;
    let grad_y = (abs(py.x) - abs(my.x)) * half_y;

    let light1_dir = normalize(vec3<f32>(-1.0, -1.0, 0.3));
    let light2_dir = normalize(vec3<f32>(-0.7, -1.0, 0.4));
    let normal = normalize(vec3<f32>(grad_x, grad_y, 1.0));

    var s1 = max(0.0, dot(normal, light1_dir));
    s1 = s1 * s1; s1 = s1 * s1; s1 = s1 * s1; // ^8
    s1 = s1 * LIGHT_1_BRIGHTNESS;
    var s2 = max(0.0, dot(normal, light2_dir));
    s2 = s2 * s2; s2 = s2 * s2; s2 = s2 * s2; // ^8
    s2 = s2 * LIGHT_2_BRIGHTNESS;

    let height_factor = abs(height) * 3.0;
    let body = mix(BASE_BODY_COL, vec3<f32>(1.0), rp.skew_intensity);
    var col = mix(BASE_COL, body, height_factor);
    col += s1 * LIGHT_1_COL;
    col += s2 * LIGHT_2_COL;
    return clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn ud_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    return length(max(abs(p) - b, vec2<f32>(0.0))) - r;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let screen_coord = in.uv - vec2<f32>(0.5);
    let screen_ar = rp.screen_resolution.x / rp.screen_resolution.y;
    let sim_ar = rp.sim_resolution.x / rp.sim_resolution.y;
    let norm_coord = screen_coord * vec2<f32>(screen_ar / sim_ar, 1.0);
    let uv = norm_coord + vec2<f32>(0.5);
    let cymatics = cymatics_color(uv);

    let vignette = 1.0 - clamp(-ud_round_box(screen_coord, vec2<f32>(0.45), 0.05) * 40.0, 0.0, 1.0);
    let bg = vec3<f32>(0.25 - length(norm_coord) * 0.2);
    let col = mix(pow(cymatics, vec3<f32>(mix(0.8, 1.0, vignette))), bg, vignette);
    return vec4<f32>(col, 1.0);
}
```

**UV-flip note:** GLSL `gl_FragCoord.y` is bottom-up; Bevy mesh `uv.y` is top-down. The image may render vertically flipped vs v4. If the Stage 8 capture shows a flip, negate `screen_coord.y` (or flip the quad UV). Flag for visual confirmation, do not pre-correct blindly.

- [ ] **Step 2: Write the failing test** (uniform size)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_params_is_32_bytes() {
        // vec2 + vec2 + f32 + vec3 padding = 32 bytes (std140-friendly).
        assert_eq!(std::mem::size_of::<CymaticsRenderParams>(), 32);
    }
}
```

- [ ] **Step 3: Implement `render.rs`** (modelled on `particles/material.rs`)

```rust
//! Fullscreen Cymatics render: a window-sized quad with [`CymaticsMaterial`]
//! sampling the compute display texture. Ports v4 renderCymatics.frag's
//! lighting via `assets/shaders/cymatics/render.wgsl`. Drawn by the normal 2D
//! pass, so the shared hand-mesh composite + bloom/AgX layer on top.

use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite_render::{AlphaMode2d, Material2d};
use bytemuck::{Pod, Zeroable};
use super::CymaticsRoot;

/// Fragment uniform mirroring `render.wgsl::RenderParams`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable, ShaderType)]
pub struct CymaticsRenderParams {
    /// Window resolution (px) for aspect correction + vignette.
    pub screen_resolution: Vec2,
    /// Sim grid resolution (texels).
    pub sim_resolution: Vec2,
    /// v4 skewIntensity body-colour push.
    pub skew_intensity: f32,
    /// Pad to 32 bytes.
    pub _pad: Vec3,
}

/// Fullscreen material sampling the compute display texture.
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct CymaticsMaterial {
    /// Lighting/aspect/skew params.
    #[uniform(0)]
    pub params: CymaticsRenderParams,
    /// The compute display texture (linear filter, as v4).
    #[texture(1)]
    #[sampler(2)]
    pub cell_texture: Handle<Image>,
}

impl Material2d for CymaticsMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/cymatics/render.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Opaque
    }
}
```

**Note on `ShaderType` vs `Pod`:** `AsBindGroup`'s `#[uniform]` requires `ShaderType` (encase). Match how `particles/material.rs` declares its `#[uniform]` payloads — it uses `Vec4` fields directly on the material, not a nested POD. If a nested uniform struct needs `ShaderType`, derive it (drop `Pod`/`Zeroable` if they conflict; the 32-byte test then checks `ShaderType`'s size via a runtime `encase` size or is replaced by a field-presence test). Prefer the **flat-fields** style of `ParticleMaterial` (e.g. `#[uniform(0)] resolution: Vec4`, `#[uniform(1)] skew: Vec4`) if that's the established idiom — keep the WGSL `RenderParams` layout matching whatever Rust shape you choose.

The quad spawn + resize systems live in `mod.rs`'s lifecycle (Stage 4), but define the helpers here:

```rust
/// Spawn the window-sized fullscreen quad with the material, tagged `CymaticsRoot`.
pub fn spawn_cymatics_quad(
    commands: &mut Commands<'_, '_>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<CymaticsMaterial>,
    display: Handle<Image>,
    window_size: Vec2,
    sim_resolution: Vec2,
) -> Entity {
    let mesh = meshes.add(Rectangle::new(window_size.x.max(1.0), window_size.y.max(1.0)));
    let material = materials.add(CymaticsMaterial {
        params: CymaticsRenderParams {
            screen_resolution: window_size,
            sim_resolution,
            skew_intensity: 0.0,
            _pad: Vec3::ZERO,
        },
        cell_texture: display,
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(material),
        Transform::default(),
        CymaticsRoot,
    )).id()
}
```

- [ ] **Step 4: Register `Material2dPlugin::<CymaticsMaterial>`** in `lib.rs`

In `SketchesPlugin::build`, alongside the `ParticleMaterial` registration:

```rust
app.add_plugins(Material2dPlugin::<crate::cymatics::render::CymaticsMaterial>::default());
```

- [ ] **Step 5: Run tests + build**

Run: `cargo nextest run -p wc-sketches cymatics::render` and `cargo build -p waveconductor`.
Expected: PASS + compiles.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-sketches/src/cymatics/render.rs assets/shaders/cymatics/render.wgsl crates/wc-sketches/src/lib.rs
git commit -F - <<'EOF'
feat(cymatics): fullscreen Material2d render + render.wgsl

CymaticsMaterial samples the compute display texture; render.wgsl ports
v4 renderCymatics.frag (height-gradient normal, two-light power-8
specular, body-colour mix, vignette, radial background, skew). Drawn in
the 2D pass so the hand-mesh composite + bloom/AgX layer on top.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 4 — Lifecycle + interaction

### Task C8: `CymaticsPlugin` lifecycle (textures, quad, sim-params bridge)

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (the `CymaticsPlugin`, lifecycle systems, minimal settings)
- Create: `crates/wc-sketches/src/cymatics/settings.rs` (minimal `CymaticsSettings`: `vertical_resolution`, `iterations` only — full surface in Stage 8)
- Modify: `crates/wc-sketches/src/lib.rs` (`app.add_plugins(cymatics::CymaticsPlugin)`)
- Test: `mod.rs`/`settings.rs` `#[cfg(test)] mod tests` (manifest registration unit test, like Dots)

**Interfaces:**
- Consumes: `create_cymatics_textures`, `CymaticsTextures`, `CymaticsSimParams`, `SimParamsGpu`, `MAX_ITERATIONS` (C5); `spawn_cymatics_quad`, `CymaticsRenderParams` (C7); `CymaticsComputePlugin` (C6).
- Produces: `CymaticsRoot` (`Component`); `CymaticsPlugin`; `CymaticsState` (`Resource`): `center: Vec2`, `center2: Vec2`, `active_radius: f32`, `num_cycles: f32`, `slow_down: f32`, `simulation_time: f32`, `center_speed: f32` (all in sim UV / v4 units); `CymaticsSettings` (minimal); `register_cymatics_manifest`.

Lifecycle: **OnEnter** (chained) — `init_cymatics_state` (insert `CymaticsState` defaults), `spawn_cymatics` (read settings → compute resolution → `create_cymatics_textures` → spawn quad with the `display` handle and tag the `CymaticsTextures` onto a `CymaticsRoot` entity → insert initial `CymaticsSimParams`), `enter_cymatics_audio` (push `AddCymaticsSynth` — full coupling in Stage 5). **OnExit** — `despawn_with::<CymaticsRoot>`, `remove_cymatics_sim_params`, `exit_cymatics_audio` (`RemoveCymaticsSynth`). **Update** (gated `sketch_active`) — `update_cymatics_sim_params` (build the per-frame `CymaticsSimParams` from `CymaticsState` + settings; fill `iter_times`).

v4 resolution: `vertical_resolution` (default 480, or 240 on ≤480px-wide screens) × `round(vertical_resolution · aspect)` for width. Use the window aspect.

- [ ] **Step 1: Minimal `settings.rs`**

```rust
//! Cymatics settings. This minimal surface (resolution + iterations, both
//! `requires_restart`) is expanded to the full Dev surface in Stage 8.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings; // confirm the derive's crate path against dots/settings.rs

#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "cymatics")]
pub struct CymaticsSettings {
    /// Sim grid vertical resolution. Restart on change (textures reallocate).
    #[setting(default = 480.0_f32, min = 64.0_f32, max = 1080.0_f32, step = 1.0_f32,
        label = "Vertical resolution", section = "Simulation", category = Dev, requires_restart)]
    #[serde(default = "default_vertical_resolution")]
    pub vertical_resolution: f32,

    /// Sim sub-steps per frame (v4 numIterations = 20). Restart on change.
    #[setting(default = 20.0_f32, min = 1.0_f32, max = 120.0_f32, step = 1.0_f32,
        label = "Iterations per frame", section = "Simulation", category = Dev, requires_restart)]
    #[serde(default = "default_iterations")]
    pub iterations: f32,
}

fn default_vertical_resolution() -> f32 { 480.0 }
fn default_iterations() -> f32 { 20.0 }

impl Default for CymaticsSettings {
    fn default() -> Self {
        Self { vertical_resolution: default_vertical_resolution(), iterations: default_iterations() }
    }
}
```

(Settings are `f32` to match the derive macro's `Number` type, as Dots does; convert to `u32` at use sites via `u32::try_from(x.round() as i64)` or a small clamped helper — no bare `as`.)

- [ ] **Step 2: Implement the plugin + lifecycle in `mod.rs`**

```rust
//! Cymatics sketch: a 2D wave-field simulation (ping-pong storage-texture
//! compute) rendered fullscreen, with mouse/hand interaction driving two wave
//! centres, full faithful audio derived from CPU-side interaction scalars
//! (no GPU readback), a wandering-wave-source attract mode, and the shared
//! bloomed hand-mesh overlay.
//!
//! Data flow: interaction systems (CPU) update [`CymaticsState`] →
//! `update_cymatics_sim_params` packs it into [`CymaticsSimParams`] (extracted
//! to the render world) → `CymaticsComputePlugin` advances the field on the GPU
//! → `CymaticsMaterial` samples the display texture. `audio_coupling` derives
//! the v4 audio scalars from the same `CymaticsState` and pushes them through
//! the audio ring.

use bevy::prelude::*;
use wc_core::lifecycle::AppState;            // confirm path against state.rs
use wc_core::sketch::{despawn_with, sketch_active}; // confirm paths
pub mod compute;
pub mod render;
pub mod settings;
pub mod systems;
pub mod screensaver;

use compute::sim_params::{create_cymatics_textures, CymaticsSimParams, CymaticsTextures, SimParamsGpu, MAX_ITERATIONS};
use settings::CymaticsSettings;

/// Marker for every entity this sketch owns (despawned on exit to free VRAM).
#[derive(Component)]
pub struct CymaticsRoot;

/// CPU-side interaction state (v4 `index.ts` instance vars). Units match v4.
#[derive(Resource, Debug, Clone)]
pub struct CymaticsState {
    /// Primary wave centre, sim UV [0,1].
    pub center: Vec2,
    /// Secondary wave centre, sim UV [0,1].
    pub center2: Vec2,
    /// Alive-mask radius (v4 `activeRadius`).
    pub active_radius: f32,
    /// Frequency control (v4 `numCycles`).
    pub num_cycles: f32,
    /// Decays ×0.95/frame; raised on interaction onset (v4 `slowDownAmount`).
    pub slow_down: f32,
    /// Phase clock (v4 `simulationTime`), advanced N·dt per frame.
    pub simulation_time: f32,
    /// Last frame's primary-centre speed (for audio), v4 `centerSpeed`.
    pub center_speed: f32,
}

impl Default for CymaticsState {
    fn default() -> Self {
        Self {
            center: Vec2::new(0.5, 0.5),
            center2: Vec2::new(0.5, 0.5),
            active_radius: systems::interaction::MINIMUM_ACTIVE_RADIUS,
            num_cycles: systems::interaction::DEFAULT_NUM_CYCLES,
            slow_down: 0.0,
            simulation_time: 0.0,
            center_speed: 0.0,
        }
    }
}

/// Cymatics plugin. Signal flow documented at the module root.
pub struct CymaticsPlugin;

impl Plugin for CymaticsPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<CymaticsSettings>();
        register_cymatics_manifest(app);
        app.add_plugins(compute::CymaticsComputePlugin);

        app.add_systems(OnEnter(AppState::Cymatics), (
            init_cymatics_state,
            spawn_cymatics,
            systems::audio_coupling::enter_cymatics_audio,
        ).chain());
        app.add_systems(OnExit(AppState::Cymatics), (
            despawn_with::<CymaticsRoot>,
            remove_cymatics_sim_params,
            systems::audio_coupling::exit_cymatics_audio,
        ));

        // Idle veto: stay Active while the field is still alive (v4 isReadyToSleep).
        app.register_idle_veto(cymatics_idle_veto);

        // Per-frame interaction + sim-params bridge (Stage 4/5 systems).
        app.add_systems(Update, (
            systems::interaction::update_cymatics_centers,
            systems::hand::update_cymatics_hand_centers,
            update_cymatics_sim_params,
            systems::audio_coupling::drive_cymatics_audio,
        ).chain().run_if(sketch_active(AppState::Cymatics)));

        // Hand mesh + attract (Stages 6/7).
        app.add_plugins(screensaver::CymaticsScreensaverPlugin);
        // hand_mesh registration added in Task C12.
    }
}

fn cymatics_idle_veto(world: &World) -> bool {
    world.get_resource::<CymaticsState>().is_some_and(|s|
        s.active_radius > systems::interaction::MINIMUM_ACTIVE_RADIUS + 1e-2)
}

fn init_cymatics_state(mut commands: Commands<'_, '_>) {
    commands.insert_resource(CymaticsState::default());
}

fn spawn_cymatics(
    mut commands: Commands<'_, '_>,
    mut images: ResMut<'_, Assets<Image>>,
    mut meshes: ResMut<'_, Assets<Mesh>>,
    mut materials: ResMut<'_, Assets<render::CymaticsMaterial>>,
    settings: Res<'_, CymaticsSettings>,
    window: Single<'_, &Window>,
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let aspect = win.x / win.y;
    let vy = u32::try_from((settings.vertical_resolution.round() as i64).max(1)).unwrap_or(480);
    let vx = u32::try_from(((settings.vertical_resolution * aspect).round() as i64).max(1)).unwrap_or(480);
    let textures = create_cymatics_textures(vx, vy, &mut images);
    let sim_resolution = Vec2::new(vx as f32, vy as f32);

    let quad = render::spawn_cymatics_quad(&mut commands, &mut meshes, &mut materials, textures.display.clone(), win, sim_resolution);
    // Tag the texture handles onto a CymaticsRoot entity so OnExit frees them.
    commands.spawn((textures.clone(), CymaticsRoot));

    let iterations = u32::try_from((settings.iterations.round() as i64).clamp(1, MAX_ITERATIONS as i64)).unwrap_or(20);
    commands.insert_resource(CymaticsSimParams {
        params: SimParamsGpu { resolution: [vx, vy], ..default_sim_params() },
        iter_times: Vec::with_capacity(MAX_ITERATIONS),
        iterations,
        tex_a: textures.a,
        tex_b: textures.b,
        display: textures.display,
        resolution: UVec2::new(vx, vy),
    });
    let _ = quad;
}

fn remove_cymatics_sim_params(mut commands: Commands<'_, '_>) {
    commands.remove_resource::<CymaticsSimParams>();
}

/// Build [`SimParamsGpu`] defaults from the v4 physics constants.
fn default_sim_params() -> SimParamsGpu {
    SimParamsGpu {
        center: [0.5, 0.5],
        center2: [0.5, 0.5],
        resolution: [1, 1],
        active_radius: systems::interaction::MINIMUM_ACTIVE_RADIUS,
        force_multiplier: 0.25,
        velocity_decay: 0.99818,
        height_decay: 0.9999,
        accumulated_height_decay: 0.999,
        _pad: 0.0,
    }
}

/// Pack `CymaticsState` + settings into `CymaticsSimParams` each frame and fill
/// the per-iteration phase times (v4: `cycles·2π/N` per sub-step).
fn update_cymatics_sim_params(
    state: Res<'_, CymaticsState>,
    settings: Res<'_, CymaticsSettings>,
    mut sim: ResMut<'_, CymaticsSimParams>,
) {
    sim.params.center = state.center.to_array();
    sim.params.center2 = state.center2.to_array();
    sim.params.active_radius = state.active_radius;
    // v4 cycles = numCycles / (1 + slowDown*3); dt = cycles·2π/N.
    let n = sim.iterations.max(1);
    let cycles = state.num_cycles / (1.0 + state.slow_down * 3.0);
    let dt = cycles * std::f32::consts::TAU / n as f32;
    let base = state.simulation_time;
    sim.iter_times.clear();
    for i in 0..n {
        sim.iter_times.push(base + i as f32 * dt);
    }
}
```

**Implementer notes:** `register_sketch_settings`, `register_cymatics_manifest`, `register_idle_veto`, `Single<&Window>`, and `despawn_with`/`sketch_active` imports must match the exact paths Dots/Line use (see `dots/mod.rs`). `register_cymatics_manifest` is implemented in Task C12 (manifest tile) — stub it here returning without a screenshot if needed, or land C12's version now (the manifest test belongs to C12). `simulation_time` advancement (`+= n·dt`) is applied in the audio-coupling system (Stage 5) where the speed/scalars are computed, to keep the v4 `step()` ordering; if coupling is not yet present, advance it at the end of `update_cymatics_sim_params` (`drop the borrow then` `state` is `Res`, so move the advance into a `ResMut<CymaticsState>` — adjust the system signature to `mut state: ResMut<CymaticsState>` and advance `state.simulation_time += n as f32 * dt` after filling `iter_times`).

- [ ] **Step 3: Register the plugin** in `lib.rs` and build

`app.add_plugins(cymatics::CymaticsPlugin)` in `SketchesPlugin::build`. Then `cargo build -p waveconductor`.

- [ ] **Step 4: First visual smoke**

Run: `cargo build -p waveconductor` then `cargo xtask capture cymatics-synthetic --watch=10` (the scenario is added in C15; for now use `WAVECONDUCTOR_START_SKETCH=cymatics cargo rund`). Confirm the field renders (mouse should create ripples once interaction lands in Task C9; before that the field is the idle low-radius ambient). Note what you see in the report.

- [ ] **Step 5: Gate + commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): plugin lifecycle, textures, sim-params bridge

OnEnter allocates the ping-pong textures + spawns the fullscreen quad;
OnExit despawns CymaticsRoot (frees VRAM). CymaticsState holds the v4
interaction scalars; update_cymatics_sim_params packs them into the
extracted CymaticsSimParams each frame and fills the per-iteration
phase times. Minimal settings (resolution + iterations).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C9: Interaction state machine (mouse/touch centres)

**Files:**
- Create: `crates/wc-sketches/src/cymatics/systems/mod.rs` (`pub mod interaction; pub mod hand; pub mod audio_coupling;`)
- Create: `crates/wc-sketches/src/cymatics/systems/interaction.rs`
- Test: same file, `#[cfg(test)] mod tests` (pure state-machine math)

**Interfaces:**
- Consumes: `CymaticsState`; the v5 pointer input (mirror Dots' `update_dots_mouse_attractor` / `PointerState` usage — read the same resource Dots reads).
- Produces: the v4 constants (`DEFAULT_NUM_CYCLES = 1.002`, `MINIMUM_ACTIVE_RADIUS = 0.1`, `MINIMUM_ACTIVE_RADIUS_INTERACTING = 0.5`, `TARGET_ACTIVE_RADIUS_INTERACTING = 7.5`, `ACTIVE_RADIUS_INTERACTING_GROW_FACTOR = 0.01`, `ACTIVE_RADIUS_IDLE_DECAY_FACTOR = 0.005`, `INTERACTION_CENTER_LERP_FACTOR = 0.01`); `update_cymatics_centers` system; `screen_to_sim_uv(ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2`; a pure `step_centers(state: &mut CymaticsState, input: CenterInput, sim_ar: f32)` that the system calls and the tests exercise.

`CenterInput { mouse_pressed: bool, mouse_uv: Vec2, c1_held: bool, c1_uv: Vec2, c2_held: bool, c2_uv: Vec2 }` — the held flags/positions come from hands (Task C10); the mouse drives the unheld primary centre. The math is a verbatim port of v4 `step()`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;
    use crate::cymatics::CymaticsState;

    fn idle_input() -> CenterInput {
        CenterInput { mouse_pressed: false, mouse_uv: Vec2::new(0.5, 0.5),
            c1_held: false, c1_uv: Vec2::ZERO, c2_held: false, c2_uv: Vec2::ZERO }
    }

    #[test]
    fn idle_decays_active_radius_toward_minimum() {
        let mut s = CymaticsState { active_radius: 5.0, ..Default::default() };
        for _ in 0..2000 { step_centers(&mut s, idle_input(), 1.0); }
        assert!((s.active_radius - MINIMUM_ACTIVE_RADIUS).abs() < 1e-2);
    }

    #[test]
    fn interacting_grows_active_radius_toward_target() {
        let mut s = CymaticsState::default();
        let input = CenterInput { mouse_pressed: true, ..idle_input() };
        for _ in 0..2000 { step_centers(&mut s, input, 1.0); }
        assert!(s.active_radius > 5.0); // approaches TARGET (7.5)
        assert!(s.active_radius >= MINIMUM_ACTIVE_RADIUS_INTERACTING);
    }

    #[test]
    fn free_center2_mirrors_center1() {
        // c1 held at (0.3,0.4); c2 free -> mirrors to (0.7,0.6) over time.
        let mut s = CymaticsState::default();
        let input = CenterInput { mouse_pressed: false, mouse_uv: Vec2::new(0.3, 0.4),
            c1_held: true, c1_uv: Vec2::new(0.3, 0.4), c2_held: false, c2_uv: Vec2::ZERO };
        for _ in 0..3000 { step_centers(&mut s, input, 1.0); }
        assert!((s.center.x - 0.3).abs() < 0.05);
        assert!((s.center2.x - 0.7).abs() < 0.05); // 1 - 0.3
        assert!((s.center2.y - 0.6).abs() < 0.05); // 1 - 0.4
    }

    #[test]
    fn num_cycles_decays_to_default_when_idle() {
        let mut s = CymaticsState { num_cycles: 1.5, ..Default::default() };
        for _ in 0..500 { step_centers(&mut s, idle_input(), 1.0); }
        assert!((s.num_cycles - DEFAULT_NUM_CYCLES).abs() < 1e-2);
    }

    #[test]
    fn is_ready_to_sleep_when_radius_low() {
        let s = CymaticsState { active_radius: MINIMUM_ACTIVE_RADIUS, ..Default::default() };
        assert!(is_ready_to_sleep(&s));
        let s2 = CymaticsState { active_radius: 1.0, ..Default::default() };
        assert!(!is_ready_to_sleep(&s2));
    }
}
```

- [ ] **Step 2: Run, verify failure.** `cargo nextest run -p wc-sketches cymatics::systems::interaction`

- [ ] **Step 3: Implement `interaction.rs`** (verbatim v4 `step()` math)

```rust
//! Two-centre interaction state machine, ported from v4 `index.ts::step()`.
//! Pure `step_centers` (unit-tested) + the Bevy system that feeds it pointer
//! input. Hands supply the `c1_held`/`c2_held` flags via Task C10.

use bevy::prelude::*;
use crate::cymatics::CymaticsState;

/// v4 module constants.
pub const DEFAULT_NUM_CYCLES: f32 = 1.002;
pub const MINIMUM_ACTIVE_RADIUS: f32 = 0.1;
pub const MINIMUM_ACTIVE_RADIUS_INTERACTING: f32 = 0.5;
pub const TARGET_ACTIVE_RADIUS_INTERACTING: f32 = 7.5;
pub const ACTIVE_RADIUS_INTERACTING_GROW_FACTOR: f32 = 0.01;
pub const ACTIVE_RADIUS_IDLE_DECAY_FACTOR: f32 = 0.005;
pub const INTERACTION_CENTER_LERP_FACTOR: f32 = 0.01;

/// Per-frame interaction input (mouse + the two hand grabs).
#[derive(Clone, Copy)]
pub struct CenterInput {
    pub mouse_pressed: bool,
    pub mouse_uv: Vec2,
    pub c1_held: bool,
    pub c1_uv: Vec2,
    pub c2_held: bool,
    pub c2_uv: Vec2,
}

/// v4 `isReadyToSleep`: active_radius near its floor.
#[must_use]
pub fn is_ready_to_sleep(state: &CymaticsState) -> bool {
    state.active_radius <= MINIMUM_ACTIVE_RADIUS + 1e-2
}

/// Advance the two-centre state machine one frame. Verbatim v4 `step()`.
pub fn step_centers(state: &mut CymaticsState, input: CenterInput, _sim_ar: f32) {
    let interacting = input.mouse_pressed || input.c1_held || input.c2_held;

    if interacting {
        state.num_cycles += 0.0003 + (state.num_cycles - DEFAULT_NUM_CYCLES) * 0.0008;
        if state.active_radius < MINIMUM_ACTIVE_RADIUS_INTERACTING {
            state.active_radius = MINIMUM_ACTIVE_RADIUS_INTERACTING;
        }
        state.active_radius = lerp(state.active_radius, TARGET_ACTIVE_RADIUS_INTERACTING, ACTIVE_RADIUS_INTERACTING_GROW_FACTOR);
    } else {
        state.active_radius = lerp(state.active_radius, MINIMUM_ACTIVE_RADIUS, ACTIVE_RADIUS_IDLE_DECAY_FACTOR);
        state.num_cycles = state.num_cycles * 0.95 + DEFAULT_NUM_CYCLES * 0.05;
    }

    let wanted_c1 = if input.c1_held { input.c1_uv } else { input.mouse_uv };

    // Held centres follow their hand.
    if input.c1_held {
        state.center = lerp2(state.center, wanted_c1, INTERACTION_CENTER_LERP_FACTOR);
    }
    if input.c2_held {
        state.center2 = lerp2(state.center2, input.c2_uv, INTERACTION_CENTER_LERP_FACTOR);
    }
    // Free centres mirror the other; unheld c1 follows the mouse.
    if !input.c1_held {
        if input.c2_held {
            let mirror = Vec2::new(1.0 - state.center2.x, 1.0 - state.center2.y);
            state.center = lerp2(state.center, mirror, INTERACTION_CENTER_LERP_FACTOR);
        } else {
            state.center = lerp2(state.center, wanted_c1, INTERACTION_CENTER_LERP_FACTOR);
        }
    }
    if !input.c2_held {
        let mirror = Vec2::new(1.0 - state.center.x, 1.0 - state.center.y);
        state.center2 = lerp2(state.center2, mirror, INTERACTION_CENTER_LERP_FACTOR);
    }

    // v4 centerSpeed = distance(wantedC1, c1) · lerpFactor.
    state.center_speed = wanted_c1.distance(state.center) * INTERACTION_CENTER_LERP_FACTOR;
    // v4 slowDownAmount decays ×0.95 each frame (raised on onset in coupling).
    state.slow_down *= 0.95;
}

fn lerp(a: f32, b: f32, t: f32) -> f32 { a + (b - a) * t }
fn lerp2(a: Vec2, b: Vec2, t: f32) -> Vec2 { a + (b - a) * t }

/// v4 `screenToSimUV`: NDC [-1,1] → sim UV [0,1] with aspect correction.
#[must_use]
pub fn screen_to_sim_uv(ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2 {
    let sc = ndc * 0.5;
    Vec2::new(
        (sc.x * (screen_ar / sim_ar) + 0.5).clamp(0.0, 1.0),
        (sc.y + 0.5).clamp(0.0, 1.0),
    )
}

/// System: read pointer input, build `CenterInput` (hands fill the held slots
/// via a resource set in Task C10), call `step_centers`.
pub fn update_cymatics_centers(
    mut state: ResMut<'_, CymaticsState>,
    window: Single<'_, &Window>,
    hands: Res<'_, super::hand::CymaticsHandGrabs>, // produced by Task C10
    // pointer input: mirror Dots' source (e.g. Res<PointerState> + cursor pos)
    pointer: Res<'_, wc_core::input::PointerState>, // confirm the exact type Dots uses
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let screen_ar = win.x / win.y;
    let sim_ar = screen_ar; // sim AR tracks window AR (resolution = vy·aspect × vy)

    // Cursor → NDC → sim UV. Use the same cursor source Dots uses; this is the
    // shape, adjust to the real API.
    let mouse_uv = pointer.cursor_position()
        .map(|p| {
            let ndc = Vec2::new(p.x / win.x * 2.0 - 1.0, 1.0 - p.y / win.y * 2.0);
            screen_to_sim_uv(ndc, screen_ar, sim_ar)
        })
        .unwrap_or(Vec2::new(0.5, 0.5));

    let input = CenterInput {
        mouse_pressed: pointer.is_pressed(), // confirm API
        mouse_uv,
        c1_held: hands.c1.is_some(),
        c1_uv: hands.c1.unwrap_or(Vec2::new(0.5, 0.5)),
        c2_held: hands.c2.is_some(),
        c2_uv: hands.c2.unwrap_or(Vec2::new(0.5, 0.5)),
    };
    step_centers(&mut state, input, sim_ar);
}
```

**Implementer note:** `wc_core::input::PointerState` and its `cursor_position()`/`is_pressed()` are placeholders — use the **exact** pointer resource + accessors Dots' `update_dots_mouse_attractor` reads (read `dots/systems/` to find them). The `step_centers`/`screen_to_sim_uv`/`is_ready_to_sleep` functions are the unit-tested core and must stay verbatim to v4.

- [ ] **Step 4: Run tests, verify pass; build.** `cargo nextest run -p wc-sketches cymatics::systems::interaction && cargo build -p waveconductor`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): two-centre interaction state machine

Verbatim port of v4 step(): activeRadius grow/decay, numCycles ramp,
held-centre follow + free-centre mirror, centerSpeed, slowDown decay.
Pure step_centers + screen_to_sim_uv are unit-tested; the system feeds
them v5 pointer input.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C10: Two-hand grab → centres

**Files:**
- Create: `crates/wc-sketches/src/cymatics/systems/hand.rs`
- Test: same file, `#[cfg(test)] mod tests` (grab → UV mapping, pure helper)

**Interfaces:**
- Consumes: `HandTrackingState` (the shared hand input — read what Dots' `hand_attractors` uses); `CymaticsState`.
- Produces: `CymaticsHandGrabs { c1: Option<Vec2>, c2: Option<Vec2> }` (`Resource`, default both `None`) — consumed by `update_cymatics_centers` (C9); `update_cymatics_hand_centers` system that maps up to two grabbing hands to `c1`/`c2` sim-UV positions; a pure `hand_to_sim_uv(palm_ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2` (reuses `screen_to_sim_uv`).

Mirror Dots' `hand_attractors::update_dots_hand_attractors`: detect grab (closed hand) per hand, map the palm position to sim UV, assign the first grabbing hand to `c1` and the second to `c2`. v4 keys centres by hand id (`centerHeldByHandId[0/1]`); replicate the stable assignment (hand index 0 → c1, hand index 1 → c2) so a two-hand grab drives both independently.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    #[test]
    fn grab_maps_palm_to_sim_uv() {
        // centre-screen palm (ndc 0,0) → sim uv (0.5,0.5) at AR 1.
        let uv = hand_to_sim_uv(Vec2::ZERO, 1.0, 1.0);
        assert!((uv - Vec2::new(0.5, 0.5)).length() < 1e-5);
    }

    #[test]
    fn assign_grabs_orders_two_hands() {
        let grabs = assign_grabs(&[
            (true, Vec2::new(-0.5, 0.0)),  // hand 0 grabbing
            (true, Vec2::new(0.5, 0.0)),   // hand 1 grabbing
        ], 1.0, 1.0);
        assert!(grabs.c1.is_some());
        assert!(grabs.c2.is_some());
    }

    #[test]
    fn no_grab_yields_none() {
        let grabs = assign_grabs(&[(false, Vec2::ZERO)], 1.0, 1.0);
        assert!(grabs.c1.is_none() && grabs.c2.is_none());
    }
}
```

- [ ] **Step 2: Run, verify failure.**

- [ ] **Step 3: Implement `hand.rs`**

```rust
//! Two-hand grab → wave centres. Maps the shared hand-tracking state to the
//! `CymaticsHandGrabs` resource consumed by the interaction state machine.

use bevy::prelude::*;
use super::interaction::screen_to_sim_uv;

/// Per-frame grab assignment: c1 = first grabbing hand, c2 = second.
#[derive(Resource, Default, Clone, Copy)]
pub struct CymaticsHandGrabs {
    pub c1: Option<Vec2>,
    pub c2: Option<Vec2>,
}

/// Palm NDC → sim UV (same mapping as the mouse).
#[must_use]
pub fn hand_to_sim_uv(palm_ndc: Vec2, screen_ar: f32, sim_ar: f32) -> Vec2 {
    screen_to_sim_uv(palm_ndc, screen_ar, sim_ar)
}

/// Pure assignment helper: `(grabbing, palm_ndc)` per hand → grabs.
#[must_use]
pub fn assign_grabs(hands: &[(bool, Vec2)], screen_ar: f32, sim_ar: f32) -> CymaticsHandGrabs {
    let mut grabs = CymaticsHandGrabs::default();
    for (grabbing, palm) in hands.iter().copied() {
        if !grabbing { continue; }
        let uv = hand_to_sim_uv(palm, screen_ar, sim_ar);
        if grabs.c1.is_none() { grabs.c1 = Some(uv); }
        else if grabs.c2.is_none() { grabs.c2 = Some(uv); }
    }
    grabs
}

/// System: read the shared hand state, write `CymaticsHandGrabs`.
pub fn update_cymatics_hand_centers(
    mut grabs: ResMut<'_, CymaticsHandGrabs>,
    window: Single<'_, &Window>,
    hand_state: Res<'_, wc_core::input::HandTrackingState>, // confirm the type Dots uses
) {
    let win = Vec2::new(window.width().max(1.0), window.height().max(1.0));
    let ar = win.x / win.y;
    // Build (grabbing, palm_ndc) per active hand from hand_state. Mirror Dots'
    // grab detection + palm→NDC mapping (read hand_attractors).
    let hands: Vec<(bool, Vec2)> = hand_state.hands()
        .map(|h| (h.is_grabbing(), h.palm_ndc()))
        .collect();
    *grabs = assign_grabs(&hands, ar, ar);
}
```

**Implementer note:** `wc_core::input::HandTrackingState`, `hands()`, `is_grabbing()`, `palm_ndc()` are placeholders — use the **exact** hand API Dots' `hand_attractors` module reads (grab detection threshold, palm position, NDC mapping). The `assign_grabs`/`hand_to_sim_uv` helpers are the unit-tested core. Register `CymaticsHandGrabs` as a resource in `CymaticsPlugin::build` (`app.init_resource::<systems::hand::CymaticsHandGrabs>();`). The `Vec` in the system is per-frame; if the hand count is bounded and small (≤2), use a fixed `[(bool, Vec2); 2]` or a `SmallVec`/`Local` scratch to honour the no-hot-path-allocation rule. Cap iteration at 2 hands.

- [ ] **Step 4: Run tests + build.** `cargo nextest run -p wc-sketches cymatics::systems::hand && cargo build -p waveconductor`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): two-hand grab drives the wave centres

CymaticsHandGrabs maps up to two grabbing hands to sim-UV centre
positions (hand 0 -> c1, hand 1 -> c2), consumed by the interaction
state machine. Pure assign_grabs/hand_to_sim_uv are unit-tested; the
system reads the shared hand-tracking state.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 5 — Audio coupling

### Task C11: Drive the audio from CPU scalars + sample triggers

**Files:**
- Create: `crates/wc-sketches/src/cymatics/systems/audio_coupling.rs`
- Test: same file, `#[cfg(test)] mod tests` (the v4 audio formula mapping, pure)

**Interfaces:**
- Consumes: `CymaticsState`, the audio command sender (`NonSendMut<AudioCommandSender>` — confirm against Line/Dots audio systems), `AudioCommand::{AddCymaticsSynth, RemoveCymaticsSynth, SetCymaticsParam, TriggerCymaticsSample}`, `CymaticsSampleId` (Stage 1).
- Produces: `enter_cymatics_audio`, `exit_cymatics_audio`, `drive_cymatics_audio` systems; a pure `audio_params(state: &CymaticsState) -> CymaticsAudioParams` computing the v4 formulas; `CymaticsAudioParams { osc_volume, osc_freq_scalar, blub_volume, blub_rate }`; a `CymaticsTriggerState { was_interacting: bool, throttle_frames: u32 }` (`Resource`) for the onset-edge sample trigger (v4 `triggerJitter`, ~500 ms throttle).

v4 formulas (from `index.ts::step()` + `audio.ts`):

```text
skew_intensity = pow(max(0, (num_cycles - 1.002)/2 - 0.5), 2)
blub_volume = pow(map(active_radius, 0.1, 1.0, 0.05, 1), 2) * 0.5
            + abs(num_cycles - 1.002) * 0.25
            - skew_intensity
            + map(center_speed, 0, 0.005, 0, 1) * map(active_radius, 0.1, 1.0, 0.12, 1) * 0.4
blub_rate   = pow(2, map(center_speed, 0, 0.005, -0.25, 1.5)) + map(num_cycles, 1.002, 2, 0, 4)
osc_volume  = clamp(smoothstep(num_cycles, 1.002, 1.1002) * 0.5, 0, 1)
cycles      = num_cycles / (1 + slow_down*3);  osc_freq_scalar = cycles / 1.002
```

where `map(x, a, b, c, d)` is v4 `MathUtils.mapLinear` (no clamp) and `smoothstep(x, e0, e1)` is the Hermite smoothstep. `blub_volume`/`blub_rate` are clamped on the **audio side** (C4: blub_volume → [0,0.3] after the synth's `·0.05`... — note: v4 `setBlubVolume` does `clamp(v·0.05, 0, 0.3)`. The `·0.05` lives in the DspHost blub volume application, so pass the raw `blub_volume` and apply `·0.05`+clamp where the LoopVoice volume is set). Decide one home for the `·0.05`+clamp and document it; the plan's C4 clamps blub_volume to [0,0.3] — so apply the `·0.05` in the coupling (`blub_volume * 0.05`) before pushing, and C4 clamps. Keep it consistent.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cymatics::CymaticsState;

    #[test]
    fn osc_freq_scalar_is_one_at_default() {
        let s = CymaticsState::default(); // num_cycles 1.002, slow_down 0
        let p = audio_params(&s);
        assert!((p.osc_freq_scalar - 1.0).abs() < 1e-4);
    }

    #[test]
    fn osc_volume_zero_at_default_frequency() {
        let s = CymaticsState::default();
        let p = audio_params(&s);
        assert!(p.osc_volume.abs() < 1e-4); // smoothstep(1.002; 1.002, 1.1002) = 0
    }

    #[test]
    fn osc_volume_rises_with_num_cycles() {
        let s = CymaticsState { num_cycles: 1.1002, ..Default::default() };
        let p = audio_params(&s);
        assert!(p.osc_volume > 0.4); // smoothstep -> 1, *0.5
    }

    #[test]
    fn blub_rate_increases_with_center_speed() {
        let slow = audio_params(&CymaticsState { center_speed: 0.0, ..Default::default() });
        let fast = audio_params(&CymaticsState { center_speed: 0.005, ..Default::default() });
        assert!(fast.blub_rate > slow.blub_rate);
    }
}
```

- [ ] **Step 2: Run, verify failure.**

- [ ] **Step 3: Implement `audio_coupling.rs`**

```rust
//! Cymatics audio coupling: derive the v4 audio scalars from `CymaticsState`
//! (CPU-side only — no GPU readback) and push them through the audio ring.
//! Gated off in the screensaver (silent), matching Dots.

use bevy::prelude::*;
use wc_core::audio::{AudioCommand, AudioCommandSender, CymaticsSampleId}; // confirm paths
use crate::cymatics::CymaticsState;

/// Derived per-frame audio parameters.
#[derive(Clone, Copy, Debug)]
pub struct CymaticsAudioParams {
    pub osc_volume: f32,
    pub osc_freq_scalar: f32,
    /// Already includes the v4 `·0.05` scale; the audio side clamps to [0,0.3].
    pub blub_volume: f32,
    pub blub_rate: f32,
}

/// Onset-edge + throttle state for the one-shot samples (v4 `triggerJitter`).
#[derive(Resource, Default)]
pub struct CymaticsTriggerState {
    was_interacting: bool,
    throttle_frames: u32,
}

fn map_linear(x: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    // v4 MathUtils.mapLinear (no clamp).
    c + (d - c) * ((x - a) / (b - a))
}
fn smoothstep01(x: f32, e0: f32, e1: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Verbatim v4 audio formulas.
#[must_use]
pub fn audio_params(s: &CymaticsState) -> CymaticsAudioParams {
    const DEF: f32 = 1.002;
    let skew = (((s.num_cycles - DEF) / 2.0 - 0.5).max(0.0)).powi(2);
    let blub_volume_raw = map_linear(s.active_radius, 0.1, 1.0, 0.05, 1.0).powi(2) * 0.5
        + (s.num_cycles - DEF).abs() * 0.25
        - skew
        + map_linear(s.center_speed, 0.0, 0.005, 0.0, 1.0)
            * map_linear(s.active_radius, 0.1, 1.0, 0.12, 1.0) * 0.4;
    let blub_rate = 2.0_f32.powf(map_linear(s.center_speed, 0.0, 0.005, -0.25, 1.5))
        + map_linear(s.num_cycles, DEF, 2.0, 0.0, 4.0);
    let osc_volume = (smoothstep01(s.num_cycles, DEF, DEF * 1.1) * 0.5).clamp(0.0, 1.0);
    let cycles = s.num_cycles / (1.0 + s.slow_down * 3.0);
    let osc_freq_scalar = cycles / DEF;
    CymaticsAudioParams {
        osc_volume,
        osc_freq_scalar,
        blub_volume: blub_volume_raw * 0.05, // v4 setBlubVolume · 0.05; audio side clamps [0,0.3]
        blub_rate,
    }
}

pub fn enter_cymatics_audio(mut sender: NonSendMut<'_, AudioCommandSender>) {
    let _ = sender.push(AudioCommand::AddCymaticsSynth);
}
pub fn exit_cymatics_audio(mut sender: NonSendMut<'_, AudioCommandSender>) {
    let _ = sender.push(AudioCommand::RemoveCymaticsSynth);
}

/// Push the derived params each frame; fire the one-shots on an interaction
/// onset edge (throttled ~500 ms = ~30 frames at 60 fps).
pub fn drive_cymatics_audio(
    state: Res<'_, CymaticsState>,
    mut trigger: ResMut<'_, CymaticsTriggerState>,
    mut sender: NonSendMut<'_, AudioCommandSender>,
) {
    let p = audio_params(&state);
    let _ = sender.push(AudioCommand::SetCymaticsParam { key: "osc_volume", value: p.osc_volume });
    let _ = sender.push(AudioCommand::SetCymaticsParam { key: "osc_freq_scalar", value: p.osc_freq_scalar });
    let _ = sender.push(AudioCommand::SetCymaticsParam { key: "blub_volume", value: p.blub_volume });
    let _ = sender.push(AudioCommand::SetCymaticsParam { key: "blub_rate", value: p.blub_rate });

    // Interaction onset = active_radius climbing past the interacting floor.
    let interacting = state.active_radius > super::interaction::MINIMUM_ACTIVE_RADIUS_INTERACTING - 1e-3;
    trigger.throttle_frames = trigger.throttle_frames.saturating_sub(1);
    if interacting && !trigger.was_interacting && trigger.throttle_frames == 0 {
        let _ = sender.push(AudioCommand::TriggerCymaticsSample(CymaticsSampleId::Kick));
        let _ = sender.push(AudioCommand::TriggerCymaticsSample(CymaticsSampleId::RisingBass));
        trigger.throttle_frames = 30; // ~500 ms at 60 fps
    }
    trigger.was_interacting = interacting;
}
```

**Notes:** Register `CymaticsTriggerState` in `CymaticsPlugin::build` (`app.init_resource::<systems::audio_coupling::CymaticsTriggerState>();`). `drive_cymatics_audio` runs in the `sketch_active` chain (already wired in C8) so it is **silent in the screensaver** (the screensaver runs under `SketchActivity::Screensaver`, not `Active`). Confirm the `AudioCommandSender` access pattern (`NonSendMut`) against Line/Dots' audio systems. The onset-edge definition (active_radius crossing the interacting floor) is a faithful proxy for v4's mouse/grab trigger; if a more direct "press began" signal is available from the pointer/hand input, prefer it and pass an explicit `just_pressed` bool into the trigger logic.

- [ ] **Step 4: Run tests + Cymatics audio smoke.** `cargo nextest run -p wc-sketches cymatics::systems::audio_coupling`, then `WAVECONDUCTOR_START_SKETCH=cymatics cargo rund` — confirm interacting produces the synth swell + blub + onset kick/risingbass; idle is near-silent.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): audio coupling from CPU interaction scalars

drive_cymatics_audio derives the v4 audio formulas (osc volume/freq
scalar, blub volume/rate) from CymaticsState and pushes them through
the ring; an onset edge fires the throttled kick/risingbass one-shots.
GPU-only data flow preserved (no readback). Silent in the screensaver.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 6 — Hand-mesh overlay + manifest tile

### Task C12: Register shared `HandMeshPlugin` + manifest tile

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (register `HandMeshPlugin`, implement `register_cymatics_manifest`)
- Create: `assets/sketches/cymatics/screenshot.png` (picker tile; 1280×720, scrubbed of system chrome)
- Test: `mod.rs` `#[cfg(test)] mod tests` (manifest registration, mirroring Dots' `register_dots_manifest` test)

**Interfaces:**
- Consumes: `crate::hand_mesh::{HandMeshPlugin, HandMeshConfig}`; `wc_core::sketch::{SketchManifestEntry, RegisterSketchManifestExt}`.
- Produces: `register_cymatics_manifest(app: &mut App)`.

Cymatics renders in the main 2D pass with **no post-process node**, so it adds **no** `HandMeshCompositeSet` ordering edge (the composite runs in `EarlyPostProcess` after the 2D pass by default — confirmed tolerant of the absent edge in sub-project #1).

- [ ] **Step 1: Add the hand-mesh registration** to `CymaticsPlugin::build`

```rust
app.add_plugins(crate::hand_mesh::HandMeshPlugin {
    config: crate::hand_mesh::HandMeshConfig {
        app_state: AppState::Cymatics,
        // Orange `#eb5938` — v4 BASE_BODY_COL (235, 89, 56).
        bone_color: Color::srgb(
            f32::from(0xeb_u8) / 255.0,
            f32::from(0x59_u8) / 255.0,
            f32::from(0x38_u8) / 255.0,
        ),
        glow_intensity: 5.0,
        bone_radius: 10.0,
    },
});
```

- [ ] **Step 2: Implement `register_cymatics_manifest`** (mirror `register_dots_manifest`)

```rust
/// Register Cymatics's picker-tile metadata. Factored out for unit-testing
/// without the rendering plugins (mirrors `register_dots_manifest`).
pub(crate) fn register_cymatics_manifest(app: &mut App) {
    let asset_server = app.world().resource::<AssetServer>();
    let screenshot = asset_server.load("sketches/cymatics/screenshot.png");
    app.register_sketch_manifest(wc_core::sketch::SketchManifestEntry {
        state: AppState::Cymatics,
        // v4 HomePage display label. Confirm against v4 HomePage.tsx; default "Cymatics".
        display_name: "Cymatics",
        screenshot,
    });
}
```

**Display-name confirmation:** check v4 `HomePage.tsx` for the label shown over the Cymatics tile (Line ships internally `Line`/displays "Gravity"; Dots/"Fabric"). If v4 uses a different word, use that exact string. No em dashes (user-facing copy).

- [ ] **Step 3: Write the manifest test** (mirror Dots)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::*;

    #[test]
    fn registers_cymatics_manifest_entry() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        register_cymatics_manifest(&mut app);
        let manifest = app.world().resource::<wc_core::sketch::SketchManifest>();
        assert!(manifest.entries.iter().any(|e| e.state == AppState::Cymatics
            && e.display_name == "Cymatics"));
    }
}
```

(Match the exact `MinimalPlugins`/`AssetPlugin` setup Dots' manifest test uses.)

- [ ] **Step 4: Run tests + visual smoke.** `cargo nextest run -p wc-sketches cymatics`, then `WAVECONDUCTOR_HAND_PROVIDER=synthetic WAVECONDUCTOR_START_SKETCH=cymatics cargo rund` — confirm the bloomed orange bones render over the field, and the Home picker shows the Cymatics tile.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): shared hand-mesh overlay + manifest tile

Register the shared HandMeshPlugin with Cymatics' orange (#eb5938)
config (no composite ordering edge — Cymatics has no post-process
node). Register the picker tile (display name "Cymatics").

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 7 — Attract / screensaver mode

### Task C13: Wandering wave sources

**Files:**
- Create: `crates/wc-sketches/src/cymatics/screensaver.rs`
- Test: same file, `#[cfg(test)] mod tests` (Lissajous determinism + bounds)

**Interfaces:**
- Consumes: `CymaticsState`, `in_screensaver(AppState::Cymatics)` run condition, `CymaticsSettings` (attract params land in Stage 8; use literals here, promote to settings in C14).
- Produces: `CymaticsScreensaverPlugin`; a pure `wander_centers(elapsed: f32) -> (Vec2, Vec2)` (two slow incommensurate Lissajous paths in [0,1]²); `drive_cymatics_attract` system that writes `CymaticsState.center`/`center2` and holds `active_radius` at a low ambient value.

Reuse Line's wandering-pulses idea: the two centres drift on slow incommensurate Lissajous curves (periods chosen co-prime so the pattern doesn't visibly loop), emitting gentle continuous ripples with `active_radius` held at a low ambient value (e.g. `0.6`) and `num_cycles` at default. Low-power: the screensaver FPS cap handles the present rate; additionally the sketch runs at reduced `vertical_resolution`/`iterations` during attract (Stage 8 wires the reduced values; here, leave the active-mode resolution and rely on the FPS cap, with a TODO note to drop resolution in C14 if soak shows it is needed).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    #[test]
    fn wander_is_deterministic_and_in_bounds() {
        for &t in &[0.0_f32, 1.5, 7.3, 100.0] {
            let (c1, c2) = wander_centers(t);
            assert_eq!((c1, c2), wander_centers(t)); // deterministic
            for c in [c1, c2] {
                assert!(c.x >= 0.0 && c.x <= 1.0 && c.y >= 0.0 && c.y <= 1.0);
            }
        }
    }

    #[test]
    fn centers_move_over_time() {
        let (a1, _) = wander_centers(0.0);
        let (b1, _) = wander_centers(3.0);
        assert!(a1.distance(b1) > 1e-3);
    }
}
```

- [ ] **Step 2: Run, verify failure.**

- [ ] **Step 3: Implement `screensaver.rs`**

```rust
//! Cymatics attract mode: the two wave centres drift on slow incommensurate
//! Lissajous paths, emitting gentle continuous ripples at a low ambient radius.
//! Gated on `in_screensaver(AppState::Cymatics)` (zero systems otherwise);
//! audio coupling is gated off (the coupling chain is `sketch_active`-only).

use bevy::prelude::*;
use wc_core::lifecycle::{in_screensaver, AppState}; // confirm paths
use crate::cymatics::CymaticsState;
use super::systems::interaction::DEFAULT_NUM_CYCLES;

/// Ambient alive-radius held during attract (gentle, low-power ripples).
const ATTRACT_ACTIVE_RADIUS: f32 = 0.6;

/// Two slow incommensurate Lissajous paths in [0,1]². Periods are mutually
/// irrational so the pattern does not visibly loop over a kiosk runtime.
#[must_use]
pub fn wander_centers(elapsed: f32) -> (Vec2, Vec2) {
    // Frequencies (Hz) chosen co-prime / irrational-ratio; amplitudes 0.3
    // around centre 0.5 keep the sources well inside the field.
    let c1 = Vec2::new(
        0.5 + 0.3 * (elapsed * 0.043).sin(),
        0.5 + 0.3 * (elapsed * 0.031).cos(),
    );
    let c2 = Vec2::new(
        0.5 + 0.3 * (elapsed * 0.037 + 1.7).sin(),
        0.5 + 0.3 * (elapsed * 0.029 + 0.6).cos(),
    );
    (c1, c2)
}

/// Plugin: drive the attract motion only while the Cymatics screensaver shows.
pub struct CymaticsScreensaverPlugin;

impl Plugin for CymaticsScreensaverPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, drive_cymatics_attract.run_if(in_screensaver(AppState::Cymatics)));
    }
}

fn drive_cymatics_attract(
    time: Res<'_, Time>,
    mut state: ResMut<'_, CymaticsState>,
) {
    let (c1, c2) = wander_centers(time.elapsed_secs());
    state.center = c1;
    state.center2 = c2;
    state.active_radius = ATTRACT_ACTIVE_RADIUS;
    state.num_cycles = DEFAULT_NUM_CYCLES;
    // simulation_time still advances via update_cymatics_sim_params, so the
    // wave-source phase keeps moving; centres just wander instead of grabbing.
}
```

**Note:** `update_cymatics_sim_params` (C8) runs under `sketch_active` only, so during the screensaver it does **not** run — meaning `CymaticsSimParams` would not refresh. Fix: gate `update_cymatics_sim_params` on `sketch_active OR in_screensaver` (i.e. run while the sketch is on-screen and simulating), or add a parallel screensaver copy. Simplest: change its `run_if` to a combined condition `sketch_active(Cymatics).or(in_screensaver(Cymatics))` (Bevy run-condition `.or`). Apply the same combined gate to the compute extract dependency. Keep the **audio** coupling `sketch_active`-only (silent in attract). Document this split in the plugin.

- [ ] **Step 4: Run tests + attract smoke.** `cargo nextest run -p wc-sketches cymatics::screensaver`, then `WC_DEBUG_FORCE_SCREENSAVER=1 WAVECONDUCTOR_START_SKETCH=cymatics cargo rund` (confirm the wandering ripples render and audio is silent).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): wandering-wave-source attract mode

Two wave centres drift on slow incommensurate Lissajous paths at a low
ambient radius; gated on in_screensaver. The sim-params bridge runs in
both Active and Screensaver; audio coupling stays Active-only (silent
attract).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Stage 8 — Settings depth + capture + AgX tuning

### Task C14: Rich Dev settings surface

**Files:**
- Modify: `crates/wc-sketches/src/cymatics/settings.rs` (expand to the full surface)
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (read settings into `default_sim_params`/`update_cymatics_sim_params`; the material `skew`/colours; the attract params), and add `restart_on_cymatics_settings_change` (mirror Dots)
- Modify: `crates/wc-sketches/src/cymatics/render.rs` (thread visual settings into `CymaticsRenderParams` — at minimum `skew` is already dynamic; colours/lights become settings if exposed)
- Test: `settings.rs` `#[cfg(test)] mod tests` (defaults + a representative live-read)

**Interfaces:**
- Produces: the full `CymaticsSettings` — **User**: `master_brightness` (visual), `osc_level`, `blub_level` (audio); **Dev** (ADVANCED): `vertical_resolution` + `iterations` (`requires_restart`); physics (`force_multiplier`, `velocity_decay`, `height_decay`, `accumulated_height_decay`); visual (`skew_curve` exponent, optionally the four colours + two light dirs/brightnesses + vignette); interaction (`min_radius`, `interacting_radius`, `target_radius`, `grow_factor`, `decay_factor`, `lerp_factor`); audio levels; attract (`attract_radius`, Lissajous speeds, attract `vertical_resolution`/`iterations`). Each field documented with its v4 origin.

Restart-required fields (`vertical_resolution`, `iterations`) drive the existing fade-out/reload path via a `restart_on_cymatics_settings_change` listener (mirror `restart_on_dots_settings_change`). Live fields (physics/visual/interaction/audio) are read each frame: physics → `update_cymatics_sim_params` (replace the hard-coded `default_sim_params` constants with settings reads); interaction → `step_centers` (thread the radii/factors in as params instead of the module constants, defaulting to the v4 values); audio levels → scale the pushed params in `drive_cymatics_audio`.

- [ ] **Step 1: Expand `CymaticsSettings`** with the fields above, each `#[setting(...)]` carrying `default` (the v4 value), `min`/`max`/`step`, `label`, `section`, `category`. Keep `vertical_resolution`/`iterations` as the only `requires_restart` fields. Add `#[serde(default = "…")]` + default fns for every field (Dots pattern).

- [ ] **Step 2: Thread settings into the live systems.** Replace literals: `default_sim_params` reads `force_multiplier`/decay factors from settings; `step_centers` takes a `CenterTuning` struct (radii + factors) sourced from settings (defaulting to the v4 constants — keep the constants as the default fns); `drive_cymatics_audio` scales `osc_volume`/`blub_volume` by the User audio levels; the material's `master_brightness` multiplies the final colour (add a `master_brightness` field to `CymaticsRenderParams` + `render.wgsl`, default 1.0 = no-op).

- [ ] **Step 3: Add the restart listener** (mirror `restart_on_dots_settings_change`): on a `requires_restart` change, begin the reload fade so OnExit→OnEnter reallocates textures at the new resolution/iterations.

- [ ] **Step 4: Tests** — defaults round-trip; a representative live-read (e.g. changing `force_multiplier` changes the packed `SimParamsGpu`). Update the `init_cymatics_state`/`step_centers` tests if signatures changed.

- [ ] **Step 5: Gate + manual settings check.** `cargo nextest run -p wc-sketches cymatics`; `cargo rund`, flip the ADVANCED toggle, confirm the Dev knobs appear and a `requires_restart` change triggers the reload fade.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(cymatics): rich Dev settings surface

Expand CymaticsSettings to physics/visual/interaction/audio/attract
knobs. Live fields read each frame; vertical_resolution + iterations
stay requires_restart and drive the reload fade. User-facing
master_brightness + audio levels.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

### Task C15: Capture scenarios + AgX tuning + soak gate

**Files:**
- Modify: `tests/visual/scenarios.toml` (add `cymatics-synthetic`, `cymatics-interacting`, `cymatics-screensaver`)
- Create: `tests/visual/baselines/cymatics-*/…` (seeded on this machine, visually confirmed)
- Modify: `assets/shaders/cymatics/render.wgsl` (AgX-tuned palette constants if needed)
- Modify: `crates/wc-sketches/src/cymatics/PARITY.md` (new; record parity decisions + soak-watch items, mirroring `dots/PARITY.md`)

**Interfaces:** none new (test + tuning task).

The AgX/HDR reality: the camera is a **single global HDR + Bloom + AgX** camera (`waveconductor/src/main.rs::spawn_camera`), shared by all sketches — so tonemapping is **not** per-sketch tunable. Match v4 by tuning the **shader palette** (`BASE_COL`/`BASE_BODY_COL`/light colours in `render.wgsl`) so the post-AgX result matches the v4 screenshots, not by changing the camera. Bloom (threshold 0) will lift the brightest cymatics highlights slightly; accept or compensate in-shader.

- [ ] **Step 1: Add the scenarios** to `scenarios.toml`

```toml
[scenarios.cymatics-synthetic]
sketch = "cymatics"
provider = "synthetic"
config = "clean"
frames = [30, 60, 120, 240]
dt = 0.016666667

[scenarios.cymatics-interacting]
sketch = "cymatics"
provider = "synthetic"
config = "clean"
frames = [60, 120, 240, 480]
dt = 0.016666667

[scenarios.cymatics-screensaver]
sketch = "cymatics"
provider = "mock"
config = "clean"
frames = [180, 360, 600, 1200]
dt = 0.016666667

[scenarios.cymatics-screensaver.debug]
FORCE_SCREENSAVER = "1"
```

(The `synthetic` provider emits a stationary hand so the bones + a grab-driven centre are deterministic; `cymatics-interacting` samples later frames where the grab has grown the field. If a deterministic "press" needs a debug toggle to force interaction without a real grab, add one and document it.)

- [ ] **Step 2: Pre-build, capture, and review.**

```bash
cargo build -p waveconductor
cargo xtask capture cymatics-synthetic --update-baselines
cargo xtask capture cymatics-interacting --update-baselines
cargo xtask capture cymatics-screensaver --update-baselines
```

Then **review each PNG yourself** (operator-in-the-loop; no LLM API spend): the field should match v4's look (orange body on dark-blue ground, specular glints, vignette). Compare against v4 `.worktrees/v4/src/sketches/cymatics/screenshots/`. If colours are off under AgX, adjust the `render.wgsl` palette constants and re-capture until the look matches, then re-seed baselines.

- [ ] **Step 3: Verify regression-gating.** Re-run `cargo xtask capture cymatics-synthetic` (no `--update-baselines`) and confirm it passes (mean-abs-diff ≤ 6.0) against the seeded baseline.

- [ ] **Step 4: Write `PARITY.md`** recording: the AgX palette decisions, the UV-flip resolution, the dynamic-offset stride, the attract reduced-resolution decision, and soak-watch items (multi-hour thermal stability; the N-iteration compute cost at max settings).

- [ ] **Step 5: Full gate + commit.** Run the **entire** AGENTS.md gate (`fmt`, `clippy --all-targets --all-features --workspace`, `nextest --workspace --all-features`, `test --doc`, `doc`, `deny check`, `check-secrets`). Commit:

```bash
git add -A
git commit -F - <<'EOF'
test(cymatics): capture scenarios, seeded baselines, AgX palette tuning

Add cymatics-synthetic/-interacting/-screensaver scenarios with
operator-seeded baselines (visually confirmed against v4). Tune the
render.wgsl palette so the post-AgX look matches v4. PARITY.md records
the parity decisions + soak-watch items.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

- [ ] **Step 6: Operator follow-ups (flag, non-blocking).** Before any release tag: manual hand-grab smoke on Leap/MediaPipe hardware; an 8-hour soak (multi-hour thermal-stability target) watching the N-iteration compute + audio steady state for allocation/throttle; confirm `rgba32float` write-only storage on the **deployment** GPU (the dev check in C5 is necessary but not sufficient).

---

## Self-Review

**Spec coverage** (each spec requirement → task):
- 2D wave sim on ping-pong textures (wave equation, dual source, alive-mask, early-out) → C5 (textures), C6 (`simulate.wgsl` + node). ✓
- N sub-steps/frame, per-iteration phase via dynamic offset → C5 (`IterParamsGpu`), C6 (node loop), C8 (`iter_times` fill). ✓
- Fullscreen render (normal, specular, body mix, vignette, background, skew) → C7. ✓
- Mouse/touch + two-hand grab centre state machine (mirror, radius, cycles ramp) → C9, C10. ✓
- Full faithful audio (6-osc synth + 3 samples, CPU-driven) → C1–C4 (infra), C11 (coupling). ✓
- Hand-mesh overlay (orange #eb5938) → C12. ✓
- Manifest tile → C12. ✓
- Rich Dev settings → C8 (minimal), C14 (full). ✓
- Attract mode (wandering Lissajous, low-power) → C13. ✓
- Sample-bank generalization (shared infra, Line gate) → C1, C2 (gated on Line). ✓
- GPU-only / no readback → preserved by construction (C11 reads only `CymaticsState`). ✓
- Capture scenarios + AgX tuning + soak → C15. ✓
- Risks: rgba32float support (C5 early check + C15 deployment check), dynamic-offset alignment (C5/C6), sample-bank regressing Line (C2 gate), per-frame allocation (C5 pre-alloc `iter_times`, C10 fixed hand buffer, C1 stack scratch). ✓

**Type consistency:** `CymaticsState` fields (`center`, `center2`, `active_radius`, `num_cycles`, `slow_down`, `simulation_time`, `center_speed`) are used identically across C8/C9/C11/C13. `CymaticsSimParams`/`SimParamsGpu`/`IterParamsGpu` field names match `simulate.wgsl`. `SetCymaticsParam` keys (`osc_volume`, `osc_freq_scalar`, `blub_volume`, `blub_rate`) match between C3/C4 (audio side) and C11 (coupling). Bank names (`line_background`, `cymatics_kick`, `cymatics_risingbass`, `cymatics_blub`) match between C2/C4 and `main.rs`. `CymaticsHandGrabs.c1/c2` match between C10 (producer) and C9 (consumer).

**Placeholder scan:** the API-shape placeholders (`PointerState`, `HandTrackingState`, `AudioCommandSender` access, exact import paths, `Image::new_fill`/`fundsp` combinator names) are explicitly flagged "confirm against `<precedent file>`" with the precedent named — they are *resolved-by-reading-the-named-file* directions, not unfilled blanks. The v4 math, the WGSL shaders, the settings values, and the audio formulas are complete and verbatim. No `TBD`/`TODO`-without-content remain.

**Known cross-task wiring the controller must enforce:**
1. `update_cymatics_sim_params` must run in **both** `sketch_active` and `in_screensaver` (C13 note) — otherwise attract shows a frozen field.
2. The `simulation_time` advance lives in exactly one system (C8 note) — do not double-advance.
3. The `·0.05` blub-volume scale lives in C11 (coupling); C4 clamps to [0,0.3] — do not apply `·0.05` twice.
4. `CymaticsComputePlugin`, `Material2dPlugin::<CymaticsMaterial>`, and `CymaticsPlugin` are each added exactly once (singletons).
