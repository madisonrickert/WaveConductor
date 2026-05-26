//! Line sketch voice graph.
//!
//! Owned by [`super::dsp::DspHost`]. Built lazily on
//! [`super::command::AudioCommand::AddLineSynth`]; torn down on
//! `RemoveLineSynth`; parameter-tuned by `SetLineParam`.
//!
//! ## Audio-thread allocation policy
//!
//! Graph construction (`LineSynth::new`) allocates: it boxes a `dyn
//! AudioUnit` and clones a handful of `Arc<AtomicU32>` parameter handles.
//! This runs on the cpal data callback at the moment `AddLineSynth` is
//! drained from the command ring — i.e., exactly once per sketch
//! activation, never per buffer. Real-time correctness depends on
//! `render`/`tick_mono` doing zero allocation, which they do: every
//! `Shared::set` and `An::get_mono` call is allocation-free.
//!
//! ## Graph shape (Phase A scope)
//!
//! Phase A ships a simplified version of v4's `createAudioGroup`:
//!
//! - Three saw/square oscillators summed as the main voice
//! - White noise → variable lowpass → fixed lowshelf
//! - Sum routed through a `bandpass` whose center frequency is
//!   `bandpass_freq + lfo_gain · sin(2π·8.66·t)` (the LFO depth is the
//!   `lfo_freq` parameter scaled by v4's `× 0.06`)
//! - Cascaded through a second `bandpass` to deepen the resonance
//! - Master `source_gain` (the `volume` parameter)
//! - Final `limiter` to control peaks
//!
//! Deferred to Phase B/C/D: the chord stack (`makeChordSource`), the second
//! `highshelf` attenuation pair, the dynamics-compressor knee/ratio match,
//! and the background-mp3 mixer. The shape is faithful to v4's spine; the
//! reactivity coupling lands in Phase D.
//!
//! ## Parameter contract
//!
//! All `SetLineParam` keys are `&'static str` literals. The recognized set
//! is exposed via [`LineSynth::KNOWN_KEYS`] (kept in lockstep with the
//! `match` arm in [`LineSynth::set_param`]) so callers and tests can
//! reference the canonical list.
//!
//! Note: [`super::dsp::DspHost::apply`] intercepts the
//! `"background_volume"` key *before* forwarding to the synth, because the
//! background-sample mix is host-level state (Plan 9 Phase B), not a
//! voice-graph parameter. `background_volume` therefore does **not**
//! appear in [`LineSynth::KNOWN_KEYS`].

use fundsp::prelude::*;

/// Sample rate handed to the synth at construction. Mirrors what cpal
/// reported to [`super::dsp::DspHost::new`].
type SampleRateHz = f64;

/// Default bandpass center frequency when the host hasn't yet received a
/// `SetLineParam { key: "bandpass_freq" }`. Matches v4's initial state
/// (`filter.frequency.setValueAtTime(0, 0)` clamped above zero to avoid
/// numerical issues in the SVF; v4 immediately overrides this from the
/// reactivity loop, but our render path must be deterministic before that).
const DEFAULT_BANDPASS_HZ: f32 = 320.0;
/// Default noise lowpass cutoff. v4 starts at 0; again we clamp above zero.
const DEFAULT_NOISE_HZ: f32 = 800.0;
/// Default LFO depth (proportional to filter frequency). Matches v4's pre-
/// reactivity value (`lfoGain.gain.setValueAtTime(0)`); v4 ramps this up
/// from the reactivity loop. We seed a small value so the filter still
/// breathes if the coupling system never runs.
const DEFAULT_LFO_DEPTH: f32 = 0.0;
/// Default master volume on activation. v4 starts at 0 (sourceGain ramps
/// in from silence as the reactivity loop runs); we mirror that so a
/// freshly activated synth is audible only once the coupling system
/// drives params.
const DEFAULT_VOLUME: f32 = 0.0;

/// LFO frequency in Hz. v4 uses `8.66 Hz` literally; we hard-code the same.
const LFO_FREQUENCY_HZ: f32 = 8.66;
/// Bandpass Q value. v4: `filter.Q.setValueAtTime(2.18, 0)`.
const BANDPASS_Q: f32 = 2.18;
/// Noise lowpass Q. v4: `noiseFilter.Q.setValueAtTime(1.0, 0)`.
const NOISE_LOWPASS_Q: f32 = 1.0;
/// `volume → sourceGain` scaling. v4: `sourceGain.gain.setTargetAtTime(volume / 6, …)`.
const SOURCE_GAIN_SCALE: f32 = 1.0 / 6.0;
/// `volume → noiseGain` scaling. v4: `noiseSourceGain.gain.setTargetAtTime(volume * 0.05, …)`.
const NOISE_GAIN_SCALE: f32 = 0.05;
/// LFO depth scaling. v4: `lfoGain.gain.setTargetAtTime(freq * .06, …)` —
/// but Plan 9 routes `lfo_freq` directly, so callers pass the post-scaled
/// depth value. We store it raw.
const LFO_DEPTH_SCALE: f32 = 1.0;

/// Filter-state-variable bandpass needs a strictly positive cutoff. The SVF
/// goes numerically unstable at exactly zero; clamp inputs and defaults.
const MIN_FILTER_HZ: f32 = 1.0;

/// Smoothing time constant (seconds) for parameter changes. v4 uses 16 ms via
/// `setTargetAtTime`; we match.
const PARAM_SMOOTHING_S: f32 = 0.016;

/// Voice graph for the Line sketch.
///
/// Owns a `Box<dyn AudioUnit>` plus a handful of [`Shared`] parameter
/// handles. Per-sample driving happens via [`Self::tick_mono`]; per-
/// command parameter writes happen via [`Self::set_param`].
pub struct LineSynth {
    /// Boxed mono graph. Constructor pre-allocates internal buffers via
    /// `AudioUnit::allocate` to avoid first-buffer hitches.
    graph: Box<dyn AudioUnit>,
    /// Shared parameter handles. The graph reads these via `var(&…)`
    /// nodes; `set_param` writes them via `Shared::set`. Each `set` is a
    /// single relaxed atomic store, allocation-free.
    bandpass_freq: Shared,
    /// Drives the noise-path lowpass cutoff in Hz.
    noise_freq: Shared,
    /// LFO depth in Hz (added to `bandpass_freq` to form the modulated cutoff).
    lfo_depth: Shared,
    /// Source-mix master volume. Multiplied through `SOURCE_GAIN_SCALE` and
    /// `NOISE_GAIN_SCALE` to feed the two voice paths.
    volume: Shared,
}

impl LineSynth {
    /// All `SetLineParam` keys this synth recognizes. Anything else is
    /// logged and dropped by [`Self::set_param`].
    pub const KNOWN_KEYS: &'static [&'static str] =
        &["bandpass_freq", "noise_freq", "lfo_freq", "volume"];

    /// Build the voice graph for a given output `sample_rate`. Allocates;
    /// call only on activation (e.g. from `AudioCommand::AddLineSynth`).
    pub fn new(sample_rate: SampleRateHz) -> Self {
        // Shared parameter handles. Cloned into the graph via `var(&…)`.
        let bandpass_freq = shared(DEFAULT_BANDPASS_HZ);
        let noise_freq = shared(DEFAULT_NOISE_HZ);
        let lfo_depth = shared(DEFAULT_LFO_DEPTH);
        let volume = shared(DEFAULT_VOLUME);

        // ----- Oscillator voice mix -----
        //
        // v4: square @ 160 * 0.30 + saw @ 320 * 0.30 + saw @ 80 * 0.90.
        // We sum into a mono signal and scale by `source_gain = volume / 6`.
        let osc_mix = (square_hz(160.0) * 0.30) + (saw_hz(320.0) * 0.30) + (saw_hz(80.0) * 0.90);

        // Source gain: `volume * (1/6)`, smoothed by `follow(0.016)` to
        // match v4's `setTargetAtTime(…, 0.016)` exponential approach.
        let source_gain = (var(&volume) * SOURCE_GAIN_SCALE) >> follow(PARAM_SMOOTHING_S);

        // ----- Noise voice -----
        //
        // v4 chain: white -> noiseSourceGain -> noiseFilter (lowpass, var
        // freq) -> noiseShelf (lowshelf 2200Hz, +8dB) -> noiseGain (1.0).
        // We collapse `noiseGain` into the multiplicative `noise_gain`
        // since both are linear; v4's `noiseSourceGain` and `noiseGain`
        // multiply cleanly.
        let noise_gain = (var(&volume) * NOISE_GAIN_SCALE) >> follow(PARAM_SMOOTHING_S);
        let noise_cutoff = var(&noise_freq) >> follow(PARAM_SMOOTHING_S);

        // `bandpass<f64>()` takes (audio, freq, Q) inputs; same shape for
        // `lowpass<f64>()`. Build the noise lowpass with dynamic cutoff:
        //   (white | noise_cutoff | const(NOISE_LOWPASS_Q)) >> lowpass()
        let noise_path = (white() | noise_cutoff | dc(NOISE_LOWPASS_Q))
            >> lowpass::<f64>()
            >> lowshelf_hz::<f64>(2200.0, 1.0, db_amp(8.0));
        let noise_voice = noise_path * noise_gain;

        // ----- LFO -----
        //
        // v4: `sourceLfo` is a `sine` at 8.66 Hz, `lfoGain` is its output
        // amplitude. The LFO output is added to filter.frequency via Web
        // Audio's signal routing. We replicate by computing
        //   modulated_cutoff = bandpass_freq + lfo_depth · sin(2π·8.66·t)
        // and feeding that into both bandpass instances.
        let bp_base = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let bp_lfo = sine_hz::<f64>(LFO_FREQUENCY_HZ)
            * ((var(&lfo_depth) * LFO_DEPTH_SCALE) >> follow(PARAM_SMOOTHING_S));
        let bp_cutoff = bp_base + bp_lfo;

        // ----- Bandpass cascade -----
        //
        // (osc_mix · source_gain | bp_cutoff | const(Q)) >> bandpass()
        //                                      >> (pass | bp_cutoff_dup | const(Q)) >> bandpass()
        //
        // The second bandpass needs its own copy of the cutoff signal — we
        // can't `Clone` an `An` node, but we can clone the `Shared` handle
        // and rebuild a new modulation summer.
        let bp_base2 = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let bp_lfo2 = sine_hz::<f64>(LFO_FREQUENCY_HZ)
            * ((var(&lfo_depth) * LFO_DEPTH_SCALE) >> follow(PARAM_SMOOTHING_S));
        let bp_cutoff2 = bp_base2 + bp_lfo2;

        let voice = osc_mix * source_gain;
        let bp1 = (voice | bp_cutoff | dc(BANDPASS_Q)) >> bandpass::<f64>();
        let bp2 = (bp1 | bp_cutoff2 | dc(BANDPASS_Q)) >> bandpass::<f64>();

        // ----- Mix + limiter -----
        //
        // v4 sums noise and `filterGain · bp2` (filterGain = 0.4). The
        // dynamics-compressor stage and the two highshelf attenuators are
        // deferred to Phase D where parity tuning happens; for Phase A we
        // run a `limiter` to keep peaks in check during real-mode bring-up.
        let mix = noise_voice + bp2 * 0.4;
        let post = mix >> limiter(0.005, 0.100);

        // Build the boxed graph at the configured sample rate. `allocate`
        // pre-sizes any internal buffers so the first `tick` is hitch-free.
        let mut graph: Box<dyn AudioUnit> = Box::new(post);
        graph.set_sample_rate(sample_rate);
        graph.allocate();

        Self {
            graph,
            bandpass_freq,
            noise_freq,
            lfo_depth,
            volume,
        }
    }

    /// Apply a `SetLineParam` write. Unknown keys are warned and dropped.
    pub fn set_param(&self, key: &'static str, value: f32) {
        match key {
            "bandpass_freq" => self.bandpass_freq.set(value.max(MIN_FILTER_HZ)),
            "noise_freq" => self.noise_freq.set(value.max(MIN_FILTER_HZ)),
            "lfo_freq" => self.lfo_depth.set(value.max(0.0)),
            "volume" => self.volume.set(value.max(0.0)),
            other => {
                tracing::warn!(key = other, value, "dropping unknown SetLineParam key");
            }
        }
    }

    /// Pull one mono sample from the graph. Allocation-free.
    #[inline]
    pub fn tick_mono(&mut self) -> f32 {
        self.graph.get_mono()
    }
}

impl core::fmt::Debug for LineSynth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LineSynth")
            .field("bandpass_freq", &self.bandpass_freq.value())
            .field("noise_freq", &self.noise_freq.value())
            .field("lfo_depth", &self.lfo_depth.value())
            .field("volume", &self.volume.value())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_are_recognized() {
        let synth = LineSynth::new(48_000.0);
        // Each key in KNOWN_KEYS should mutate the corresponding shared
        // value. We verify by setting then reading back.
        synth.set_param("bandpass_freq", 1234.0);
        assert!((synth.bandpass_freq.value() - 1234.0).abs() < f32::EPSILON);
        synth.set_param("noise_freq", 567.0);
        assert!((synth.noise_freq.value() - 567.0).abs() < f32::EPSILON);
        synth.set_param("lfo_freq", 12.0);
        assert!((synth.lfo_depth.value() - 12.0).abs() < f32::EPSILON);
        synth.set_param("volume", 0.5);
        assert!((synth.volume.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_key_is_dropped_without_panic() {
        let synth = LineSynth::new(48_000.0);
        synth.set_param("absolute_nonsense", 999.0);
        // Nothing crashed and no shared param mutated.
        assert!((synth.bandpass_freq.value() - DEFAULT_BANDPASS_HZ).abs() < f32::EPSILON);
        assert!((synth.noise_freq.value() - DEFAULT_NOISE_HZ).abs() < f32::EPSILON);
        assert!((synth.lfo_depth.value() - DEFAULT_LFO_DEPTH).abs() < f32::EPSILON);
        assert!((synth.volume.value() - DEFAULT_VOLUME).abs() < f32::EPSILON);
    }

    #[test]
    fn nonpositive_filter_freq_is_clamped() {
        let synth = LineSynth::new(48_000.0);
        synth.set_param("bandpass_freq", -50.0);
        assert!(synth.bandpass_freq.value() >= MIN_FILTER_HZ);
        synth.set_param("noise_freq", 0.0);
        assert!(synth.noise_freq.value() >= MIN_FILTER_HZ);
    }

    #[test]
    fn tick_produces_audio_when_volume_set() {
        let mut synth = LineSynth::new(48_000.0);
        synth.set_param("volume", 1.0);
        synth.set_param("bandpass_freq", 320.0);
        synth.set_param("noise_freq", 800.0);
        // Run a few hundred samples to let the `follow` smoothers ramp.
        // Once `volume` smooths above ~0, the noise and oscillator paths
        // are audible. The osc voice itself is deterministic so we expect
        // *some* non-zero output by the end of the warmup.
        let mut max_abs = 0.0_f32;
        for _ in 0..2048 {
            let s = synth.tick_mono();
            if s.abs() > max_abs {
                max_abs = s.abs();
            }
        }
        assert!(
            max_abs > 0.0001,
            "expected non-silent output once volume>0, max_abs = {max_abs}"
        );
    }
}
