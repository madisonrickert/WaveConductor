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
//! - Post-mix: two cascaded `highshelf` attenuators at `BASE × 4` (1280 Hz)
//!   and `BASE × 8` (2560 Hz), each `-6 dB`, then a `limiter` for peak
//!   protection. The highshelves match v4's `highAttenuation` +
//!   `highAttenuation2` pair (added in Plan 11 Phase F after Madison's
//!   manual-test feedback that v5 had more high-frequency content than v4).
//!
//! Deferred: the chord stack (`makeChordSource`), the dynamics-compressor
//! knee/ratio match (fundsp doesn't ship a direct equivalent to Web Audio's
//! `DynamicsCompressorNode`; the `limiter` approximates the protective role),
//! and the background-mp3 mixer (which lives at host level in `DspHost`,
//! Plan 9 Phase B). The shape is faithful to v4's spine.
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

/// Default LFO rate (Hz) before the coupling system overrides it. v4
/// initialises `sourceLfo.frequency` to `8.66` but immediately overrides
/// it every frame in `index.ts::step()` with `flatRatio` (the cloud's
/// width/height ratio, typically 1–3 Hz during sustained press). We seed
/// the default at v4's typical sustained value so the LFO sounds correct
/// even before the coupling system runs.
const DEFAULT_LFO_RATE_HZ: f32 = 1.0;
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

/// v4 oscillator base frequency. Used to position the two post-mix highshelf
/// attenuators at `BASE × 4 = 1280 Hz` and `BASE × 8 = 2560 Hz`. v4:
/// `const BASE_FREQUENCY = 320` in `audio.ts`.
const BASE_FREQUENCY_HZ: f32 = 320.0;

/// Highshelf attenuation in dB, applied twice in v4's post-mix chain
/// (`highAttenuation` + `highAttenuation2`). Each stage cuts frequencies
/// above its corner by 6 dB; cascaded they produce ~12 dB attenuation above
/// the second corner (2560 Hz). Without these, v5 has noticeably more
/// high-frequency content than v4 — Madison's manual-test feedback.
const HIGHSHELF_ATTENUATION_DB: f32 = -6.0;

/// Q (resonance) for the highshelf attenuators. v4 doesn't set a Q value
/// explicitly, falling back to Web Audio's default of 1.0 (≈ no resonance,
/// gentle shelf slope). fundsp's `highshelf_hz` takes a Q parameter.
const HIGHSHELF_Q: f32 = 1.0;

// --- Plan 11 Phase F option B: generative DSP layer ---
//
// Replaces v4's particle-driven within-press swing with three time-varying
// generators inside the DSP graph: brown noise breath on voice gain, slow
// drift LFO on bandpass cutoff, slow drift LFO on noise lowpass cutoff.
// The drift LFO periods are chosen co-prime so the two filters' wanderings
// never align audibly (least common multiple ≈ 14 hours).

/// Lowpass cutoff for brown-noise breath modulator. 6 Hz keeps the modulation
/// in the "breath" range (0.5–2 Hz perceived wander), slow enough to read as
/// the voice breathing rather than amplitude tremolo.
const BREATH_LOWPASS_HZ: f32 = 6.0;

/// Q for the breath lowpass — 0.7 = no resonance, gentle slope.
const BREATH_LOWPASS_Q: f32 = 0.7;

/// Breath modulation depth (fraction of source gain). `±0.15` produces
/// source gain swings of `[0.85, 1.15]` of nominal — audible motion without
/// dipping toward silence or distorting on peaks.
const BREATH_DEPTH: f32 = 0.15;

/// Slow drift LFO frequency for the bandpass cutoff (~77 s period). At this
/// rate, one cycle takes longer than any single press; the modulation reads
/// as long-term filter wander rather than as a perceptible cycle.
const BANDPASS_DRIFT_HZ: f32 = 0.013;

/// Slow drift LFO amplitude for the bandpass cutoff. ±25 Hz on a center that
/// ranges 220–555 Hz is enough to hear but small enough not to dominate the
/// envelope-driven sweep.
const BANDPASS_DRIFT_DEPTH_HZ: f32 = 25.0;

/// Slow drift LFO frequency for the noise lowpass cutoff (~24 s period).
/// Co-prime with [`BANDPASS_DRIFT_HZ`] so the two filter wanderings never
/// align audibly over realistic listening windows.
const NOISE_DRIFT_HZ: f32 = 0.041;

/// Slow drift LFO amplitude for the noise lowpass cutoff. ±200 Hz on a center
/// that ranges 900–2000 Hz rotates the noise "formant" position perceptibly
/// over its 24 s cycle.
const NOISE_DRIFT_DEPTH_HZ: f32 = 200.0;

// --- Per-oscillator detune drift ---
//
// Each of the three oscillators (square @ 160, saw @ 320, saw @ 80) gets a
// slow sine-LFO frequency modulation at a co-prime rate. ±10 cents of detune
// at each oscillator's nominal frequency. Beats between harmonics produce
// the classic analog-synth "alive" character — the voice never sits perfectly
// in tune, harmonics phase against each other on long timescales.

/// LFO rate for oscillator #1 (square @ 160 Hz). Period ≈ 52.6 s. Co-prime
/// with the other two oscillators' drift rates.
const OSC1_DRIFT_HZ: f32 = 0.019;

/// Detune depth for oscillator #1 in Hz. ±10 cents at 160 Hz = ±0.926 Hz.
const OSC1_DRIFT_DEPTH_HZ: f32 = 0.93;

/// LFO rate for oscillator #2 (saw @ 320 Hz). Period ≈ 32.3 s.
const OSC2_DRIFT_HZ: f32 = 0.031;

/// Detune depth for oscillator #2 in Hz. ±10 cents at 320 Hz = ±1.85 Hz.
const OSC2_DRIFT_DEPTH_HZ: f32 = 1.85;

/// LFO rate for oscillator #3 (saw @ 80 Hz). Period ≈ 18.9 s.
const OSC3_DRIFT_HZ: f32 = 0.053;

/// Detune depth for oscillator #3 in Hz. ±10 cents at 80 Hz = ±0.463 Hz.
const OSC3_DRIFT_DEPTH_HZ: f32 = 0.46;

// --- Breath depth meta-modulation ---

/// Sine LFO rate for breath depth swell. Period ≈ 30 s — slow enough to feel
/// intentional, fast enough to perceive within a typical 1-minute interaction.
const BREATH_DEPTH_LFO_HZ: f32 = 0.033;

/// Amplitude of the breath-depth swell. With [`BREATH_DEPTH`] = 0.15, this
/// makes the effective depth swing between 0.10 and 0.20 — sometimes more
/// breath, sometimes calmer.
const BREATH_DEPTH_LFO: f32 = 0.05;

// --- Bandpass Q modulation ---

/// Lowpass cutoff for the brown-noise generator that modulates bandpass Q.
/// 0.5 Hz means Q wanders over ~1–3 second timescales — slower than the
/// LFO-driven cutoff wobble, faster than the slow drift LFO. Sits in the
/// "filter character morphs" perceptual band.
const BANDPASS_Q_LOWPASS_HZ: f32 = 0.5;

/// Q modulation depth. With [`BANDPASS_Q`] = 2.18, Q wanders roughly
/// `2.18 ± 0.4` (1.78–2.58). At Q=1.78 the bandpass is broader, mellower; at
/// Q=2.58 it's narrower, more ringing. Two independent brown-noise modulators
/// drive the two bandpass stages so the cascade isn't perfectly in lockstep.
const BANDPASS_Q_DEPTH: f32 = 0.4;

// --- Evolution envelope (pad-synthesis "patch develops over time") ---
//
// `evolution` is a Shared driven from CPU side (audio_coupling sends a slow
// follow on `grouped_upness`). It rises over ~4 s on press, decays over ~6 s
// on release, ranging 0..1. Used to scale modulator depths and add to the
// bandpass cutoff so the patch evolves dramatically over a held press —
// the classic pad-synthesis filter envelope.

/// Modulator-depth scaling at evolution = 0. Modulators don't go silent at
/// rest — they stay at 30% of nominal so the press onset still has texture.
/// Combined with `EVOLUTION_DELTA`, total scale ranges 0.3 (rest) to 1.0
/// (full development).
const EVOLUTION_BASE: f32 = 0.3;

/// Modulator-depth growth from evolution = 0 to 1. With `EVOLUTION_BASE = 0.3`
/// and `EVOLUTION_DELTA = 0.7`, total scale ranges 0.3..1.0.
const EVOLUTION_DELTA: f32 = 0.7;

/// Bandpass cutoff opening, in Hz, at evolution = 1.0. The filter cutoff is
/// `bandpass_freq_param + (evolution × CUTOFF_OPEN_HZ)`. At rest the cutoff
/// sits at the param value (driven by particle stats). Over a held press,
/// the cutoff opens by up to this amount, producing the classic pad "filter
/// swell" effect.
const CUTOFF_OPEN_HZ: f32 = 200.0;

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
    /// LFO oscillator rate in Hz. v4 sets this to `flatRatio` every frame —
    /// typically 1–3 Hz during sustained press, slower when the particle
    /// cloud is roughly circular, faster during left/right cursor motion.
    /// The LFO modulates `bandpass_freq` to produce the filter's audible
    /// breathing character.
    lfo_rate: Shared,
    /// LFO modulation depth in Hz (added to `bandpass_freq` to form the
    /// modulated cutoff). v4 sets this as `bandpass_freq × 0.06` inside
    /// `setFrequency`, auto-coupling depth to cutoff.
    lfo_depth: Shared,
    /// Source-mix master volume. Multiplied through `SOURCE_GAIN_SCALE` and
    /// `NOISE_GAIN_SCALE` to feed the two voice paths.
    volume: Shared,
    /// **Evolution envelope** — drives the modulator-depth growth that makes
    /// the patch develop over a sustained press (pad synthesis technique).
    /// Slow follow on press state: ~4 s attack, ~6 s release. Range \[0, 1\].
    /// Scales LFO depth, Q modulation depth, breath depth, and adds to the
    /// bandpass cutoff. At evolution = 0, modulators are at a minimal
    /// baseline; at 1.0, full dramatic depth. See
    /// [`crate::audio::line_synth::EVOLUTION_BASE`] /
    /// [`crate::audio::line_synth::EVOLUTION_DELTA`] for the scaling shape.
    evolution: Shared,
}

impl LineSynth {
    /// All `SetLineParam` keys this synth recognizes. Anything else is
    /// logged and dropped by [`Self::set_param`].
    pub const KNOWN_KEYS: &'static [&'static str] = &[
        "bandpass_freq",
        "noise_freq",
        "lfo_freq",
        "lfo_rate_hz",
        "volume",
        "evolution",
    ];

    /// Build the voice graph for a given output `sample_rate`. Allocates;
    /// call only on activation (e.g. from `AudioCommand::AddLineSynth`).
    pub fn new(sample_rate: SampleRateHz) -> Self {
        // Shared parameter handles. Cloned into the graph via `var(&…)`.
        let bandpass_freq = shared(DEFAULT_BANDPASS_HZ);
        let noise_freq = shared(DEFAULT_NOISE_HZ);
        let lfo_rate = shared(DEFAULT_LFO_RATE_HZ);
        let lfo_depth = shared(DEFAULT_LFO_DEPTH);
        let volume = shared(DEFAULT_VOLUME);
        let evolution = shared(0.0);

        // ----- Oscillator voice mix -----
        //
        // v4: square @ 160 * 0.30 + saw @ 320 * 0.30 + saw @ 80 * 0.90.
        //
        // **Per-oscillator detune drift** (Plan 11 Phase F "more organic"
        // pass): each oscillator's frequency is modulated by a slow sine LFO
        // at a co-prime rate (52 s, 32 s, 19 s periods). Depth is ±10 cents,
        // which produces audible beating between harmonics — the classic
        // analog-synth "alive" character. The three LFO rates never align
        // (their periods are mutually irrational ratios) so the detune
        // pattern doesn't loop audibly over the kiosk runtime.
        //
        // 10 cents in Hz: `f × (2^(10/1200) - 1) ≈ f × 0.00578`. So:
        //   160 Hz → ±0.93 Hz; 320 Hz → ±1.85 Hz; 80 Hz → ±0.46 Hz.
        let osc1_freq = dc(160.0) + sine_hz::<f32>(OSC1_DRIFT_HZ) * OSC1_DRIFT_DEPTH_HZ;
        let osc2_freq = dc(320.0) + sine_hz::<f32>(OSC2_DRIFT_HZ) * OSC2_DRIFT_DEPTH_HZ;
        let osc3_freq = dc(80.0) + sine_hz::<f32>(OSC3_DRIFT_HZ) * OSC3_DRIFT_DEPTH_HZ;
        let osc1 = osc1_freq >> square();
        let osc2 = osc2_freq >> saw();
        let osc3 = osc3_freq >> saw();
        let osc_mix = osc1 * 0.30 + osc2 * 0.30 + osc3 * 0.90;

        // Source gain: `volume * (1/6)`, smoothed by `follow(0.016)` to
        // match v4's `setTargetAtTime(…, 0.016)` exponential approach.
        //
        // **Organic breath modulation** (Plan 11 Phase F option B): the gain
        // is multiplied by `1 + breath × BREATH_DEPTH` where `breath` is
        // brown noise lowpassed at 6 Hz. The result is a slow 0.5–2 Hz
        // wandering of perceived loudness, gated by the gain itself (source
        // gain = 0 → modulation is silent). Adds the within-press "alive"
        // motion that v4's particle dynamics produced for free.
        let source_gain_base = (var(&volume) * SOURCE_GAIN_SCALE) >> follow(PARAM_SMOOTHING_S);
        let breath = brown::<f32>() >> lowpass_hz::<f32>(BREATH_LOWPASS_HZ, BREATH_LOWPASS_Q);
        // **Breath depth meta-modulation + evolution scaling**: breath depth
        // is `(baseline + LFO swell) × evolution_scale`, where
        // `evolution_scale = EVOLUTION_BASE + EVOLUTION_DELTA × evolution`.
        // At evolution=0 (silence or onset), breath is at EVOLUTION_BASE
        // (30%) of nominal. At evolution=1.0 (full developed press), breath
        // is at 100%. Combined with the 30 s LFO swell on the baseline,
        // breath has both short-term (LFO) and long-term (envelope) shape.
        let breath_depth_signal =
            dc(BREATH_DEPTH) + sine_hz::<f32>(BREATH_DEPTH_LFO_HZ) * BREATH_DEPTH_LFO;
        let evolution_scale_breath = dc(EVOLUTION_BASE) + var(&evolution) * EVOLUTION_DELTA;
        let source_gain =
            source_gain_base * (1.0 + breath * breath_depth_signal * evolution_scale_breath);

        // ----- Noise voice -----
        //
        // v4 chain: white -> noiseSourceGain -> noiseFilter (lowpass, var
        // freq) -> noiseShelf (lowshelf 2200Hz, +8dB) -> noiseGain (1.0).
        //
        // **v5 improvement** (Plan 11 Phase F): replace v4's white noise
        // with pink noise (`pink::<f64>()`), and drop the lowshelf boost
        // from +8 dB to +3 dB. Pink has a -3 dB/octave roll-off, producing a
        // more musical "air" spectrum than white's flat-by-design hiss. The
        // reduced shelf boost stops the 0–2200 Hz band from piling up; v4's
        // +8 dB shelf made the noise voice sound crushed and honky in the
        // low-mid (Madison's feedback). Net effect: noise reads as breath /
        // wind rather than as filtered hiss.
        let noise_gain = (var(&volume) * NOISE_GAIN_SCALE) >> follow(PARAM_SMOOTHING_S);
        // Noise cutoff base + a very slow drift LFO (~24 s period, ±200 Hz)
        // so the noise voice's formant character wanders over long timescales.
        // Period chosen co-prime with the bandpass drift (Plan 11 Phase F
        // option B) so the two never align audibly — kiosk-friendly.
        let noise_cutoff_base = var(&noise_freq) >> follow(PARAM_SMOOTHING_S);
        let noise_slow_drift = sine_hz::<f32>(NOISE_DRIFT_HZ) * NOISE_DRIFT_DEPTH_HZ;
        let noise_cutoff = noise_cutoff_base + noise_slow_drift;

        // `bandpass<f64>()` takes (audio, freq, Q) inputs; same shape for
        // `lowpass<f64>()`. Build the noise lowpass with dynamic cutoff:
        //   (white | noise_cutoff | const(NOISE_LOWPASS_Q)) >> lowpass()
        let noise_path = (pink::<f64>() | noise_cutoff | dc(NOISE_LOWPASS_Q))
            >> lowpass::<f64>()
            >> lowshelf_hz::<f64>(2200.0, 1.0, db_amp(3.0));
        let noise_voice = noise_path * noise_gain;

        // ----- LFO -----
        //
        // v4: `sourceLfo` is a sine whose frequency is set to `flatRatio`
        // (typically 1-3 Hz) each frame via `sourceLfoFreq.setTargetAtTime`,
        // and `lfoGain.gain` is set to `bandpass_freq × 0.06`. The LFO
        // output is added to filter.frequency via Web Audio's signal
        // routing. We replicate by computing
        //   modulated_cutoff = bandpass_freq + lfo_depth · sin(2π · lfo_rate · t)
        // with both `lfo_rate` and `lfo_depth` driven from `Shared` handles
        // updated by the coupling system. Use `sine` (variable-rate, takes
        // frequency from input) rather than `sine_hz` (constant rate).
        // Bandpass cutoff = follow(bandpass_freq) + LFO depth modulation
        //                   + very slow drift LFO (~77 s period, ±25 Hz).
        // The slow drift gives the filter long-form motion that doesn't loop
        // audibly (Plan 11 Phase F option B). Period chosen co-prime with
        // NOISE_DRIFT_HZ so the two filters' wanderings never align.
        // Bandpass cutoff composition:
        //   base + (LFO × evolution_scale) + slow_drift + (evolution × CUTOFF_OPEN_HZ)
        //
        // The last term is the **filter envelope**: cutoff opens by up to
        // CUTOFF_OPEN_HZ during sustained press, then slowly closes. Classic
        // pad-synthesis "swell" — the filter blooms over 4 s during attack,
        // staying open as long as the press is held.
        let bp_base = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let bp_lfo_raw = ((var(&lfo_rate) >> follow(PARAM_SMOOTHING_S)) >> sine::<f64>())
            * ((var(&lfo_depth) * LFO_DEPTH_SCALE) >> follow(PARAM_SMOOTHING_S));
        let evolution_scale_lfo1 = dc(EVOLUTION_BASE) + var(&evolution) * EVOLUTION_DELTA;
        let bp_lfo = bp_lfo_raw * evolution_scale_lfo1;
        let bp_slow_drift = sine_hz::<f32>(BANDPASS_DRIFT_HZ) * BANDPASS_DRIFT_DEPTH_HZ;
        let bp_cutoff = bp_base + bp_lfo + bp_slow_drift + var(&evolution) * CUTOFF_OPEN_HZ;

        // ----- Bandpass cascade -----
        //
        // (osc_mix · source_gain | bp_cutoff | const(Q)) >> bandpass()
        //                                      >> (pass | bp_cutoff_dup | const(Q)) >> bandpass()
        //
        // The second bandpass needs its own copy of the cutoff signal — we
        // can't `Clone` an `An` node, but we can clone the `Shared` handle
        // and rebuild a new modulation summer.
        let bp_base_stage2 = var(&bandpass_freq) >> follow(PARAM_SMOOTHING_S);
        let bp_stage2_lfo_unscaled =
            ((var(&lfo_rate) >> follow(PARAM_SMOOTHING_S)) >> sine::<f64>())
                * ((var(&lfo_depth) * LFO_DEPTH_SCALE) >> follow(PARAM_SMOOTHING_S));
        let evolution_scale_lfo2 = dc(EVOLUTION_BASE) + var(&evolution) * EVOLUTION_DELTA;
        let bp_stage2_lfo = bp_stage2_lfo_unscaled * evolution_scale_lfo2;
        let bp_stage2_drift = sine_hz::<f32>(BANDPASS_DRIFT_HZ) * BANDPASS_DRIFT_DEPTH_HZ;
        let bp_cutoff2 =
            bp_base_stage2 + bp_stage2_lfo + bp_stage2_drift + var(&evolution) * CUTOFF_OPEN_HZ;

        let voice = osc_mix * source_gain;
        // **Bandpass Q modulation × evolution**: Q drifts via brown-noise, but
        // the *modulation depth* itself grows with evolution. At press onset
        // (evolution → 0), Q is steady at BANDPASS_Q — voice has a clean,
        // focused character. As the press develops (evolution → 1), Q drift
        // grows to full ±BANDPASS_Q_DEPTH, morphing the filter's character
        // from soft to ringing on its own. Independent brown noise per stage
        // so the cascade isn't perfectly locked.
        let evolution_scale_q1 = dc(EVOLUTION_BASE) + var(&evolution) * EVOLUTION_DELTA;
        let evolution_scale_q2 = dc(EVOLUTION_BASE) + var(&evolution) * EVOLUTION_DELTA;
        let bp_q1 = dc(BANDPASS_Q)
            + (brown::<f32>() >> lowpass_hz::<f32>(BANDPASS_Q_LOWPASS_HZ, BREATH_LOWPASS_Q))
                * BANDPASS_Q_DEPTH
                * evolution_scale_q1;
        let bp_q2 = dc(BANDPASS_Q)
            + (brown::<f32>() >> lowpass_hz::<f32>(BANDPASS_Q_LOWPASS_HZ, BREATH_LOWPASS_Q))
                * BANDPASS_Q_DEPTH
                * evolution_scale_q2;
        let bp1 = (voice | bp_cutoff | bp_q1) >> bandpass::<f64>();
        let bp2 = (bp1 | bp_cutoff2 | bp_q2) >> bandpass::<f64>();

        // ----- Mix + post-process -----
        //
        // v4 sums noise and `filterGain · bp2` (filterGain = 0.4), then
        // routes through:
        //   compressor (threshold=-50, knee=12, ratio=2)
        //     → highshelf at BASE×4 (1280 Hz), -6 dB
        //     → highshelf at BASE×8 (2560 Hz), -6 dB
        //     → output.
        //
        // The two highshelves cumulatively cut content above 2560 Hz by
        // ~12 dB — a defining tonal feature of v4's sound. Without them,
        // v5 was noticeably brighter / had more high-frequency content
        // (Madison's manual-test feedback). The `limiter` stays as the
        // final peak guard (fundsp doesn't ship a direct equivalent to
        // Web Audio's DynamicsCompressorNode; the limiter approximates
        // the protective role of v4's compressor at slightly different
        // dynamics. If sign-off shows the dynamics differ audibly, swap
        // for a custom compressor in a follow-up).
        let mix = noise_voice + bp2 * 0.4;
        // `highshelf_hz::<f64>` takes f64 arguments. Our highshelf consts
        // are stored as `f32` to match the rest of the audio module; convert
        // at the call site via `f64::from` (lossless from f32 to f64).
        let post =
            mix >> highshelf_hz::<f64>(
                f64::from(BASE_FREQUENCY_HZ) * 4.0,
                f64::from(HIGHSHELF_Q),
                db_amp(f64::from(HIGHSHELF_ATTENUATION_DB)),
            ) >> highshelf_hz::<f64>(
                f64::from(BASE_FREQUENCY_HZ) * 8.0,
                f64::from(HIGHSHELF_Q),
                db_amp(f64::from(HIGHSHELF_ATTENUATION_DB)),
            ) >> limiter(0.005, 0.100);

        // Build the boxed graph at the configured sample rate. `allocate`
        // pre-sizes any internal buffers so the first `tick` is hitch-free.
        let mut graph: Box<dyn AudioUnit> = Box::new(post);
        graph.set_sample_rate(sample_rate);
        graph.allocate();

        Self {
            graph,
            bandpass_freq,
            noise_freq,
            lfo_rate,
            lfo_depth,
            volume,
            evolution,
        }
    }

    /// Apply a `SetLineParam` write. Unknown keys are warned and dropped.
    pub fn set_param(&self, key: &'static str, value: f32) {
        match key {
            "bandpass_freq" => self.bandpass_freq.set(value.max(MIN_FILTER_HZ)),
            "noise_freq" => self.noise_freq.set(value.max(MIN_FILTER_HZ)),
            // Historically named `lfo_freq` for compatibility with the Phase F
            // wiring; routed to LFO depth (in Hz). The actual LFO rate is
            // `lfo_rate_hz`.
            "lfo_freq" => self.lfo_depth.set(value.max(0.0)),
            // LFO oscillator rate. v4 typical 1-3 Hz during sustained press.
            // Clamp to a sane LFO range: below 0.1 Hz is sub-perception
            // (single-cycle longer than a typical interaction), above 20 Hz
            // crosses into low-frequency audio range and would sound like
            // FM rather than amplitude modulation.
            "lfo_rate_hz" => self.lfo_rate.set(value.clamp(0.1, 20.0)),
            "volume" => self.volume.set(value.max(0.0)),
            "evolution" => self.evolution.set(value.clamp(0.0, 1.0)),
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
            .field("lfo_rate", &self.lfo_rate.value())
            .field("lfo_depth", &self.lfo_depth.value())
            .field("volume", &self.volume.value())
            .field("evolution", &self.evolution.value())
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
        synth.set_param("lfo_rate_hz", 2.5);
        assert!((synth.lfo_rate.value() - 2.5).abs() < f32::EPSILON);
        synth.set_param("volume", 0.5);
        assert!((synth.volume.value() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn lfo_rate_hz_clamps_to_sane_range() {
        let synth = LineSynth::new(48_000.0);
        // Below 0.1 Hz: sub-perception LFO; clamp up.
        synth.set_param("lfo_rate_hz", 0.001);
        assert!((synth.lfo_rate.value() - 0.1).abs() < f32::EPSILON);
        // Above 20 Hz: low-audio range, FM not AM; clamp down.
        synth.set_param("lfo_rate_hz", 100.0);
        assert!((synth.lfo_rate.value() - 20.0).abs() < f32::EPSILON);
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
