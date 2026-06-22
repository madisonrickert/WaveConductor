//! Dots sketch voice graph.
//!
//! Faithful port of v4's `dots/audio.ts` `createAudioGroup` cascade.
//! Simpler than [`super::line_synth::LineSynth`] — no evolution envelope,
//! no brown-noise breath modulators, no per-oscillator drift. Just the v4
//! chain: two detuned triangle oscillators → lowpass → bandpass + white
//! noise, driven by a warm fixed ~1.5 Hz LFO whose depth is a `Shared` param
//! (set by the coupling system to `bandpass_freq × 0.06`).
//!
//! Note: v4's `createAudioGroup` sets `lfo.frequency = 8.66` only at
//! construction time — a placeholder that v4 overwrites every frame with
//! `flatRatio` (~1–3 Hz). The v5 voice locks the rate at 1.5 Hz (the
//! modeled-breath approach: the slow in-out swell is synthesized in the
//! coupling layer, not here).
//!
//! ## Audio-thread allocation policy
//!
//! Same as `LineSynth`: [`DotsSynth::new`] allocates (boxes the graph,
//! clones `Arc`-backed `Shared` handles); [`DotsSynth::set_param`] and
//! [`DotsSynth::tick_mono`] are allocation-free (atomic stores + a single
//! `AudioUnit::get_mono` call).
//!
//! ## Graph shape
//!
//! ```text
//! osc1 (triangle, 82.41 Hz +2 cents, ×0.3)  ─┐
//! osc2 (triangle, 164.82 Hz,          ×0.3)  ─┴─→ osc_mix
//!                                                    × follow(volume)
//!                                                    │
//!   (voice | follow(bandpass_freq) + sine(1.5)×follow(lfo_depth) | Q=5.18) >> lowpass
//!                                                    │
//!   (lp    | follow(bandpass_freq) + sine(1.5)×follow(lfo_depth) | Q=5.18) >> bandpass
//!                                                    │ ×0.7 (filterGain)
//!                                                    │
//!   white() × follow(volume × 0.05)  ───────────────┴─→ + → output
//! ```
//!
//! ## Parameter contract
//!
//! | Key              | Effect                                              |
//! |------------------|-----------------------------------------------------|
//! | `"bandpass_freq"` | Both filter cutoffs (Hz); clamped ≥ 1 Hz           |
//! | `"lfo_depth"`    | LFO depth in Hz; caller sets to `freq × 0.06`; must not exceed `bandpass_freq` — the modulated cutoff has no in-graph floor and a negative SVF cutoff is undefined |
//! | `"volume"`       | Source gain; noise gain is derived as `vol × 0.05` |
//!
//! See [`DotsSynth::KNOWN_KEYS`] for the canonical key list.

use fundsp::prelude::*;

/// Sample rate handed to the synth at construction.
type SampleRateHz = f64;

/// Base frequency for the Dots sketch (v4: `BASE_FREQUENCY = 164.82`).
const BASE_HZ: f32 = 164.82;
/// First oscillator frequency: `BASE / 2 ≈ 82.41 Hz`. v4: `detuned(BASE/2, 2)`.
const OSC1_FREQ_HZ: f32 = BASE_HZ / 2.0;
/// Second oscillator frequency: `BASE = 164.82 Hz`. v4 uses this directly.
const OSC2_FREQ_HZ: f32 = BASE_HZ;
/// +2 cents detuning factor for `osc1`: `2^(2/1200) ≈ 1.001_157`. Produces a
/// subtle chorus-like beating against `osc2` at `OSC2_FREQ_HZ`.
const OSC1_DETUNE_FACTOR: f32 = 1.001_157;
/// Filter Q for both the lowpass and bandpass stages. v4: `filter.Q = 5.18`.
const FILTER_Q: f32 = 5.18;
/// Post-bandpass amplitude scalar. v4: `filterGain.gain = 0.7`.
const FILTER_GAIN: f32 = 0.7;
/// Fixed LFO rate in Hz. v4 sets `lfo.frequency = 8.66` at construction but
/// overwrites it every frame with `flatRatio` (~1–3 Hz), so 8.66 was only a
/// placeholder. The v5 voice locks the rate at a warm 1.5 Hz; the slow
/// in-out swell is synthesized in the coupling layer instead. Not a Shared param.
const LFO_RATE_HZ: f32 = 1.5;
/// Noise path gain factor: `noise_gain = volume × NOISE_GAIN_SCALE`. v4: `volume * 0.05`.
const NOISE_GAIN_SCALE: f32 = 0.05;
/// SVF filters go numerically unstable at exactly zero cutoff; clamp any
/// incoming `bandpass_freq` to this floor before writing the Shared.
const MIN_FILTER_HZ: f32 = 1.0;
/// Parameter smoothing time constant (seconds). Mirrors v4's
/// `setTargetAtTime(_, _, 0.016)` exponential approach.
const PARAM_SMOOTHING_S: f32 = 0.016;
/// Initial bandpass cutoff. v4 sets the filter frequency to 0 at construction
/// (`filter.frequency.setValueAtTime(0, 0)`) but the first frame of the
/// reactivity loop overrides it. We seed at `MIN_FILTER_HZ` for SVF stability
/// before the first coupling write arrives.
const DEFAULT_BANDPASS_HZ: f32 = MIN_FILTER_HZ;
/// Initial LFO depth. v4: `lfoGain.gain = 0` at construction; the coupling
/// system sets it to `bandpass_freq × 0.06` each frame.
const DEFAULT_LFO_DEPTH: f32 = 0.0;
/// Initial master volume. v4: `sourceGain.gain = 0` at construction; the
/// reactivity loop ramps it up so the synth is silent until interaction.
const DEFAULT_VOLUME: f32 = 0.0;

/// Voice graph for the Dots sketch.
///
/// Owns a `Box<dyn AudioUnit>` plus three [`Shared`] parameter handles.
/// Construction allocates; [`Self::set_param`] and [`Self::tick_mono`] do not.
pub struct DotsSynth {
    /// Boxed mono DSP graph. Pre-allocated in [`DotsSynth::new`] via
    /// `AudioUnit::allocate()` to avoid first-buffer hitches.
    graph: Box<dyn AudioUnit>,
    /// Center frequency for both filter cutoffs in Hz. Clamped to
    /// `[MIN_FILTER_HZ, ∞)` on every [`Self::set_param`] write. Both the
    /// lowpass and bandpass stages read this (through independent `var` nodes)
    /// so both cutoffs track together, matching v4's `setFrequency(freq)`.
    bandpass_freq: Shared,
    /// LFO modulation depth in Hz. v4's `setFrequency` sets this as
    /// `lfoGain.gain = freq * 0.06`; the caller (coupling system) is
    /// responsible for the multiplication, this handle stores the result.
    ///
    /// Caller contract: keep `lfo_depth` ≤ `bandpass_freq` (the coupling uses
    /// `bandpass_freq × 0.06`); the modulated cutoff has no in-graph floor and
    /// a negative SVF cutoff is undefined. Mirrors `LineSynth`'s caller-enforced
    /// convention.
    lfo_depth: Shared,
    /// Source-mix master volume. The noise path derives its gain as
    /// `volume × NOISE_GAIN_SCALE` (0.05).
    volume: Shared,
}

impl DotsSynth {
    /// All `SetDotsParam` keys this synth recognizes. Anything else is
    /// logged via `tracing::warn!` and dropped by [`Self::set_param`].
    pub const KNOWN_KEYS: &'static [&'static str] = &["bandpass_freq", "lfo_depth", "volume"];

    /// Build the Dots voice graph for the given output `sample_rate`.
    /// Allocates once; call only from `AudioCommand::AddDotsSynth` dispatch.
    pub fn new(sample_rate: SampleRateHz) -> Self {
        // Shared parameter handles. The graph reads these through `var(&…)`
        // nodes; `set_param` writes them via `Shared::set`, which is a single
        // relaxed atomic store (allocation-free).
        let bandpass_freq = shared(DEFAULT_BANDPASS_HZ);
        let lfo_depth = shared(DEFAULT_LFO_DEPTH);
        let volume = shared(DEFAULT_VOLUME);

        // ----- Oscillators -----
        //
        // v4: `detuned(BASE/2, 2)` = triangle at 82.41 Hz shifted +2 cents.
        // 2 cents: 2^(2/1200) ≈ 1.001157. osc2 at BASE = 164.82 Hz, no detune.
        // Both have gain 0.30 in v4; gain is applied via the `* 0.3` scalars.
        let osc1 = dc(OSC1_FREQ_HZ * OSC1_DETUNE_FACTOR) >> triangle();
        let osc2 = dc(OSC2_FREQ_HZ) >> triangle();
        let osc_mix = osc1 * 0.3 + osc2 * 0.3;

        // ----- Source gain -----
        //
        // v4: `sourceGain.gain.setTargetAtTime(volume, _, 0.016)`.
        // `follow(PARAM_SMOOTHING_S)` is a 1-pole smoother that matches this
        // exponential approach. No `/6` scaling (LineSynth has that due to its
        // multi-oscillator loudness; Dots has a simpler two-oscillator stack).
        let source_gain = var(&volume) >> follow(PARAM_SMOOTHING_S);
        let voice = osc_mix * source_gain;

        // ----- LFO + filter cutoff (stage 1 — feeds lowpass) -----
        //
        // v4: `lfoGain.gain` is set to `freq × 0.06`; `lfoGain.connect(filter.frequency)`.
        // The filter.frequency CV = base_freq + lfo_output.
        // lfo_output = sine(LFO_RATE_HZ) × lfo_depth.
        // `sine_hz(LFO_RATE_HZ)` is a fixed-rate oscillator at 1.5 Hz
        // (LFO_RATE_HZ is a compile-time constant; no Shared needed for the rate).
        // v4 used 8.66 Hz only as a construction-time placeholder and overwrote it
        // each frame with flatRatio (~1–3 Hz); 1.5 Hz models that warm wobble range.
        let lfo1 = sine_hz::<f32>(LFO_RATE_HZ);
        let lfo_depth1 = var(&lfo_depth) >> follow(PARAM_SMOOTHING_S);
        let bp_base1 = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let cutoff1 = bp_base1 + lfo1 * lfo_depth1;

        // ----- LFO + filter cutoff (stage 2 — feeds bandpass) -----
        //
        // v4: `lfoGain` also connects to `filter2.frequency`. Both filters
        // share the same LFO signal in v4; we duplicate the expression here
        // because fundsp An-nodes are not Clone. Two `sine_hz::<f32>(LFO_RATE_HZ)`
        // nodes starting at t=0 produce identical deterministic output at 1.5 Hz,
        // so the duplication is semantically equivalent to sharing.
        let lfo2 = sine_hz::<f32>(LFO_RATE_HZ);
        let lfo_depth2 = var(&lfo_depth) >> follow(PARAM_SMOOTHING_S);
        let bp_base2 = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let cutoff2 = bp_base2 + lfo2 * lfo_depth2;

        // ----- Filter chain -----
        //
        // v4: source → lowpass (Q=5.18) → bandpass (Q=5.18) → filterGain (0.7).
        // Both filters share the same center frequency (bandpass_freq + LFO).
        // `lowpass::<f64>()` / `bandpass::<f64>()` use f64 for internal
        // coefficient math (biquad coefficients), keeping filter accuracy high
        // even when the center frequency is very low or when Q is large.
        let lp = (voice | cutoff1 | dc(FILTER_Q)) >> lowpass::<f64>();
        let bp = (lp | cutoff2 | dc(FILTER_Q)) >> bandpass::<f64>();
        let filtered_voice = bp * FILTER_GAIN;

        // ----- Noise path -----
        //
        // v4: white noise → noiseGain (volume × 0.05).
        // The noise path gives the sound its characteristic "breath" texture.
        // `follow` smooths the gain to match v4's `setTargetAtTime(_, _, 0.016)`.
        let noise_gain = (var(&volume) * NOISE_GAIN_SCALE) >> follow(PARAM_SMOOTHING_S);
        let noise_voice = white() * noise_gain;

        // ----- Mix -----
        //
        // v4: `filterGain.connect(audioContext.gain)` and
        //     `noiseGain.connect(audioContext.gain)` — both sum at the output bus.
        // No per-source limiter; the DspHost's master gain (applied in render)
        // keeps the output in range.
        let mix = filtered_voice + noise_voice;

        let mut graph: Box<dyn AudioUnit> = Box::new(mix);
        graph.set_sample_rate(sample_rate);
        // Pre-allocate internal buffers. Avoids first-callback hitches from
        // lazy allocation inside the fundsp graph.
        graph.allocate();

        Self {
            graph,
            bandpass_freq,
            lfo_depth,
            volume,
        }
    }

    /// Apply a `SetDotsParam` write. Unknown keys are logged and dropped.
    ///
    /// Every branch is a single `Shared::set` (relaxed atomic store) —
    /// allocation-free and safe to call from the audio thread.
    pub fn set_param(&self, key: &'static str, value: f32) {
        match key {
            // Clamp to MIN_FILTER_HZ: SVF filters go numerically unstable at
            // exactly 0 Hz; the coupling system may transiently write 0 before
            // the first interaction frame.
            "bandpass_freq" => self.bandpass_freq.set(value.max(MIN_FILTER_HZ)),
            // lfo_depth = bandpass_freq × 0.06 per v4; always non-negative.
            "lfo_depth" => self.lfo_depth.set(value.max(0.0)),
            // volume 0 → silence; negative volume would invert phase (not desired).
            "volume" => self.volume.set(value.max(0.0)),
            other => {
                tracing::warn!(key = other, value, "dropping unknown SetDotsParam key");
            }
        }
    }

    /// Pull one mono sample from the graph. Allocation-free.
    #[inline]
    pub fn tick_mono(&mut self) -> f32 {
        self.graph.get_mono()
    }
}

impl core::fmt::Debug for DotsSynth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DotsSynth")
            .field("bandpass_freq", &self.bandpass_freq.value())
            .field("lfo_depth", &self.lfo_depth.value())
            .field("volume", &self.volume.value())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard that the LFO rate constant stays in a perceptually warm range.
    /// 8.66 Hz (v4's construction-time placeholder) caused excess shimmer;
    /// anything above ~3 Hz would re-introduce that problem.
    #[test]
    fn lfo_rate_is_warm() {
        // Evaluated at compile time via const block; fails to compile if the
        // constant drifts out of the safe low-shimmer range.
        const { assert!(LFO_RATE_HZ > 0.0 && LFO_RATE_HZ <= 3.0) }
    }

    #[test]
    fn new_builds_without_panic() {
        // Construction must succeed and not panic at 48 kHz.
        let _synth = DotsSynth::new(48_000.0);
    }

    #[test]
    fn set_param_routes_to_shared() {
        let synth = DotsSynth::new(48_000.0);

        synth.set_param("bandpass_freq", 440.0);
        assert!(
            (synth.bandpass_freq.value() - 440.0).abs() < f32::EPSILON,
            "bandpass_freq Shared not updated"
        );

        synth.set_param("lfo_depth", 26.4);
        assert!(
            (synth.lfo_depth.value() - 26.4).abs() < f32::EPSILON,
            "lfo_depth Shared not updated"
        );

        synth.set_param("volume", 0.75);
        assert!(
            (synth.volume.value() - 0.75).abs() < f32::EPSILON,
            "volume Shared not updated"
        );
    }

    #[test]
    fn filter_freq_clamped_above_zero() {
        let synth = DotsSynth::new(48_000.0);
        // v4 starts filters at 0 Hz; the coupling system overrides on the first
        // frame. We clamp to MIN_FILTER_HZ to keep SVF stable before that.
        synth.set_param("bandpass_freq", -100.0);
        assert!(
            synth.bandpass_freq.value() >= MIN_FILTER_HZ,
            "negative bandpass_freq not clamped"
        );
        synth.set_param("bandpass_freq", 0.0);
        assert!(
            synth.bandpass_freq.value() >= MIN_FILTER_HZ,
            "zero bandpass_freq not clamped"
        );
    }

    #[test]
    fn lfo_depth_clamped_non_negative() {
        let synth = DotsSynth::new(48_000.0);
        synth.set_param("lfo_depth", -5.0);
        assert!(
            synth.lfo_depth.value() >= 0.0,
            "negative lfo_depth should be clamped to 0"
        );
    }

    #[test]
    fn unknown_key_drops_without_panic() {
        let synth = DotsSynth::new(48_000.0);
        // Unknown key must not panic or corrupt any Shared.
        synth.set_param("no_such_param", 999.0);
        assert!(
            (synth.bandpass_freq.value() - DEFAULT_BANDPASS_HZ).abs() < f32::EPSILON,
            "bandpass_freq corrupted by unknown key"
        );
        assert!(
            (synth.lfo_depth.value() - DEFAULT_LFO_DEPTH).abs() < f32::EPSILON,
            "lfo_depth corrupted by unknown key"
        );
        assert!(
            (synth.volume.value() - DEFAULT_VOLUME).abs() < f32::EPSILON,
            "volume corrupted by unknown key"
        );
    }

    #[test]
    fn volume_zero_produces_silence() {
        let mut synth = DotsSynth::new(48_000.0);
        // Default is already 0.0; explicit set to confirm.
        synth.set_param("volume", 0.0);
        let mut max_abs = 0.0_f32;
        for _ in 0..512 {
            let s = synth.tick_mono();
            if s.abs() > max_abs {
                max_abs = s.abs();
            }
        }
        assert!(
            max_abs < 1e-5,
            "expected near-silence at volume=0, max_abs = {max_abs}"
        );
    }

    #[test]
    fn volume_positive_produces_audio() {
        let mut synth = DotsSynth::new(48_000.0);
        synth.set_param("volume", 1.0);
        synth.set_param("bandpass_freq", 300.0);
        // Allow ~50 ms for the `follow(0.016)` smoothers to ramp.
        // 48 000 × 0.05 = 2 400 samples.
        let mut max_abs = 0.0_f32;
        for _ in 0..2_400 {
            let s = synth.tick_mono();
            if s.abs() > max_abs {
                max_abs = s.abs();
            }
        }
        assert!(
            max_abs > 1e-4,
            "expected non-silent output with volume=1, max_abs = {max_abs}"
        );
    }
}
