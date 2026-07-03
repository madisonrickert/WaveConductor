//! Flame sketch voice graph.
//!
//! Owned by [`super::dsp::DspHost`]. Built lazily on
//! [`super::command::AudioCommand::AddFlameSynth`]; torn down on
//! `RemoveFlameSynth`; parameter-tuned by `SetFlameParam`.
//!
//! ## Ported from v4 `FlameAudio`
//!
//! This mirrors `.worktrees/v4/src/sketches/flame/audio.ts`. Three voices sum
//! into a soft limiter (v4's `DynamicsCompressorNode` is replaced by a `tanh`
//! shaper — see the post stage):
//!
//! - **Chord voice** (`createChord`): five oscillators — root/third/fifth
//!   sines, sub (root/2) and sub2 (root/4) triangles — at fixed voice gains
//!   1.0 / 1.0 / 0.7 / 0.9 / 0.8, all scaled by a single chord gain. Each
//!   frequency is a [`Shared`] recomputed by `FlameSynth::recompute_chord`
//!   from `chord_degree` + `is_major` via the ported scale math.
//! - **Noise voice**: white noise scaled by the (smoothed) noise gain. v4's
//!   noise is unfiltered into the compressor; the name-tuned lowpass belongs to
//!   the osc voice, not the noise.
//! - **"Osc" voice — the v4 DC quirk, ported deliberately**: v4 runs a square
//!   oscillator at **0 Hz** (a constant +1) at construction gain 0.6 through
//!   the name-tuned resonant lowpass. The gain *modulation* is the audible
//!   signal; the resonant filter rings it. (v4's 0 Hz triangle sibling is a
//!   constant 0 — silent — and is not ported.)
//!
//! ## Post stage
//!
//! The summed mix runs through `shape(Tanh(1.0))` — the spec's soft limiter
//! replacing v4's `DynamicsCompressorNode` — then a smoothed master gain. The
//! `tanh` shaper bounds the output to `(-1, 1)` regardless of source levels.
//!
//! ## Audio-thread allocation policy
//!
//! Graph construction ([`FlameSynth::new`]) allocates: it boxes a `dyn
//! AudioUnit` and clones the [`Shared`] parameter handles. This runs on the
//! cpal data callback the moment `AddFlameSynth` is drained from the command
//! ring — exactly once per sketch activation, never per buffer.
//! [`FlameSynth::set_param`] and [`FlameSynth::tick_mono`] are allocation-free:
//! every `Shared::set` and `AudioUnit::get_mono` call is a plain atomic store
//! or a buffer read.
//!
//! ## Why `set_param` is `&mut self`
//!
//! Unlike [`super::line_synth::LineSynth`] (whose params are pure `Shared`
//! writes and so take `&self`), `FlameSynth` carries the v4 per-frame one-pole
//! accumulators (`noise_gain_value` / `osc_gain_value` / `chord_gain_value`)
//! and the name-config scalars (`has_noise`, `density`, `chord_degree`, …) as
//! plain fields. `morph_energy` reads and rewrites those accumulators, so the
//! method needs `&mut self`. [`super::dsp::DspHost`] matches it with
//! `&mut self.flame_synth`.
//!
//! ## Parameter contract
//!
//! All `SetFlameParam` keys are `&'static str` literals. The recognized set is
//! exposed via [`FlameSynth::KNOWN_KEYS`] (kept in lockstep with the `match`
//! arm in [`FlameSynth::set_param`]). Unknown keys are logged via
//! `tracing::warn!` and dropped — the DSP host must never panic on a stale key.

use fundsp::prelude::*;

/// Sample rate handed to the synth at construction (mirrors what cpal reported
/// to [`super::dsp::DspHost::new`]).
type SampleRateHz = f64;

/// Smoothing time constant (seconds) for parameter changes. v4 uses 16 ms via
/// `setTargetAtTime`; we match with `follow(0.016)`. This is the anti-click
/// smoother on every driven `var`.
const PARAM_SMOOTHING_S: f32 = 0.016;

/// v4 chord root frequency in Hz (`ROOT_FREQ = 120`).
const ROOT_FREQ: f32 = 120.0;

/// v4 major scale (semitone offsets within an octave).
const MAJOR_SCALE: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
/// v4 minor scale (semitone offsets within an octave).
const MINOR_SCALE: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];

/// Construction gain of the v4 "osc" voice square oscillator (`gain: 0.6`). The
/// oscillator runs at 0 Hz, so it is a constant `+0.6` before gain modulation.
const OSC_VOICE_DC: f32 = 0.6;

/// Fixed per-voice gains inside the chord (`createChord`): root, third, fifth
/// (0.7), sub (0.9), sub2 (0.8). Root and third are unity.
const CHORD_GAIN_FIFTH: f32 = 0.7;
/// Sub-octave (root/2) triangle voice gain.
const CHORD_GAIN_SUB: f32 = 0.9;
/// Two-octaves-down (root/4) triangle voice gain.
const CHORD_GAIN_SUB2: f32 = 0.8;

/// Absolute lower bound on the resonant-lowpass cutoff. The state-variable
/// filter is numerically unstable at exactly zero; clamp above it.
const MIN_FILTER_HZ: f32 = 1.0;
/// Absolute upper bound on the filter cutoff before the Nyquist clamp. The
/// per-synth ceiling is `min(FILTER_CEILING_HZ, sample_rate * 0.45)`.
const FILTER_CEILING_HZ: f32 = 18_000.0;
/// Nyquist safety fraction: keep the cutoff below `sample_rate * 0.45` so the
/// SVF never runs against the Nyquist edge.
const NYQUIST_FRACTION: f32 = 0.45;

/// Five chord-voice frequencies in Hz, derived from a scale degree.
///
/// Returned by [`chord_frequencies`]. `sub`/`sub2` are the root divided by
/// 2 and 4 (the v4 triangle sub-octaves).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChordFreqs {
    /// Root oscillator frequency (Hz).
    pub root: f32,
    /// Third oscillator frequency (Hz): scale degree + 3.
    pub third: f32,
    /// Fifth oscillator frequency (Hz): scale degree + 5.
    pub fifth: f32,
    /// Sub oscillator frequency (Hz): `root / 2`.
    pub sub: f32,
    /// Sub2 oscillator frequency (Hz): `root / 4`.
    pub sub2: f32,
}

/// Semitone number for a scale index, mirroring v4 `getSemitoneNumber`.
///
/// `octave = floor(index / 7)`, `pitch_class = index % 7`; the return is
/// `octave * 12 + scale[pitch_class]`. Negative indices are not produced by the
/// caller (scale degrees are clamped `>= 0` upstream), but `rem_euclid` keeps
/// the pitch-class lookup in range defensively.
fn semitone_number(scale_index: i32, is_major: bool) -> i32 {
    let scale = if is_major { MAJOR_SCALE } else { MINOR_SCALE };
    let len = 7;
    let octave = scale_index.div_euclid(len);
    // `rem_euclid` yields a non-negative pitch class even for negative indices.
    let pitch_class = scale_index.rem_euclid(len);
    // `pitch_class` is in `0..7`, always a valid index into `scale`.
    octave * 12 + scale[usize::try_from(pitch_class).unwrap_or(0)]
}

/// Convert a semitone number to Hz relative to [`ROOT_FREQ`] (v4 `getFreq`):
/// `120 * 2^(semitone / 12)`.
fn semitone_to_hz(semitone: i32) -> f32 {
    // Semitone counts are tiny (well within `f32`'s exact-integer range), so
    // widen through `i16` to avoid a lossy `i32 as f32`.
    let semitone_f = f32::from(i16::try_from(semitone).unwrap_or(0));
    ROOT_FREQ * 2.0_f32.powf(semitone_f / 12.0)
}

/// Compute the five chord-voice frequencies for a scale `degree`, matching v4
/// `createChord::recompute`.
///
/// `degree` is rounded to the nearest integer (v4 `setScaleDegree` does
/// `Math.round(sd)`). Third is `degree + 3`, fifth is `degree + 5`; v4's
/// `minorBias` / `fifthBias` are never driven and so are not applied. `sub` and
/// `sub2` are the root divided by 2 and 4.
#[must_use]
pub fn chord_frequencies(degree: f32, is_major: bool) -> ChordFreqs {
    // v4: `baseScaleDegree = Math.round(sd)`.
    let base = round_degree(degree);
    let root = semitone_to_hz(semitone_number(base, is_major));
    let third = semitone_to_hz(semitone_number(base + 3, is_major));
    let fifth = semitone_to_hz(semitone_number(base + 5, is_major));
    ChordFreqs {
        root,
        third,
        fifth,
        sub: root / 2.0,
        sub2: root / 4.0,
    }
}

/// Round a floating scale degree to the nearest integer without an `as` cast,
/// matching JS `Math.round` (round half up) closely enough for the whole-number
/// degrees the caller sends.
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "round()+clamp make the value an integral float in [0, 48]; the cast is exact"
)]
fn round_degree(degree: f32) -> i32 {
    // `round()` gives JS `Math.round`-like half-up behavior for the whole-number
    // degrees the caller sends; the clamp keeps this in `[0, 48]` (v4's
    // `baseOffset` range), so the `as i32` truncation of an already-integral,
    // clamped float is exact and cannot overflow.
    degree.round().clamp(0.0, 48.0) as i32
}

/// Compute the resonant-lowpass cutoff ceiling for a sample rate:
/// `min(18k, sample_rate * 0.45)` (the Nyquist-clamp carry-forward).
#[allow(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "audio sample rates fit f32 exactly; the product stays far below f32 precision limits"
)]
fn cutoff_ceiling(sample_rate: SampleRateHz) -> f32 {
    let nyquist_ceiling = (sample_rate as f32) * NYQUIST_FRACTION;
    FILTER_CEILING_HZ.min(nyquist_ceiling)
}

/// Voice graph for the Flame sketch.
///
/// Owns a `Box<dyn AudioUnit>` plus the [`Shared`] parameter handles the graph
/// reads, and the v4 mapping state (`*_gain_value` one-pole accumulators and the
/// name-config scalars). Per-sample driving happens via [`Self::tick_mono`];
/// per-command parameter writes happen via [`Self::set_param`].
pub struct FlameSynth {
    /// Boxed mono graph. The constructor pre-allocates internal buffers via
    /// `AudioUnit::allocate` to avoid first-buffer hitches.
    graph: Box<dyn AudioUnit>,

    // --- Shared parameter handles (graph reads these via `var(&…)`) ---
    /// Resonant-lowpass cutoff (Hz) for the osc voice. Set by `filter_freq`.
    filter_freq: Shared,
    /// Resonant-lowpass Q for the osc voice. Set by `filter_q`.
    filter_q: Shared,
    /// Noise-voice amplitude. Written from `noise_gain_value` each `morph_energy`.
    noise_gain: Shared,
    /// Osc-voice gain modulation. Written from `osc_gain_value`.
    osc_gain: Shared,
    /// Chord-voice master gain. Written from `chord_gain_value`.
    chord_gain: Shared,
    /// Post master gain = `camera_gain * volume_scale` (or 0 during a duck).
    master: Shared,
    /// Root oscillator frequency (Hz).
    freq_root: Shared,
    /// Third oscillator frequency (Hz).
    freq_third: Shared,
    /// Fifth oscillator frequency (Hz).
    freq_fifth: Shared,
    /// Sub oscillator frequency (Hz), `root / 2`.
    freq_sub: Shared,
    /// Sub2 oscillator frequency (Hz), `root / 4`.
    freq_sub2: Shared,

    // --- v4 mapping state (why `set_param` is `&mut self`) ---
    /// Noise-gain one-pole accumulator (v4 `noiseGain.gain.value`).
    noise_gain_value: f32,
    /// Osc-gain one-pole accumulator (v4 `oscGain.gain.value`).
    osc_gain_value: f32,
    /// Chord-gain one-pole accumulator (v4 `chord.gain.gain.value`).
    chord_gain_value: f32,
    /// Name-tuned noise-gain scale (v4 `noiseGainScale`). Multiplies velocity.
    noise_gain_scale: f32,
    /// Whether the noise voice is enabled for this name (v4 `audioHasNoise`).
    has_noise: bool,
    /// Major/minor toggle for the chord (v4 `isMajor`).
    is_major: bool,
    /// Current scale degree driving the chord (v4 `baseScaleDegree`, pre-round).
    chord_degree: f32,
    /// Fractal density used in the noise-amplitude term.
    density: f32,
    /// Chord-energy scale standing in for v4's `count^2 / 8`.
    chord_energy_scale: f32,
    /// Volume-scale factor from `volume_scale` (`context.gain` in v4).
    volume_scale: f32,
    /// Camera-distance gain factor (v4 `1/(1+dist) + 0.5`).
    camera_gain: f32,
    /// Cutoff ceiling for this sample rate: `min(18k, sample_rate * 0.45)`.
    max_filter_hz: f32,
}

impl FlameSynth {
    /// All `SetFlameParam` keys this synth recognizes. Anything else is logged
    /// and dropped by [`Self::set_param`].
    pub const KNOWN_KEYS: &'static [&'static str] = &[
        "morph_energy",
        "camera_distance",
        "volume_scale",
        "duck_pulse",
        "filter_freq",
        "filter_q",
        "noise_scale",
        "has_noise",
        "is_major",
        "chord_degree",
        "density",
        "chord_energy",
    ];

    /// Build the voice graph for a given output `sample_rate`. Allocates; call
    /// only on activation (e.g. from `AudioCommand::AddFlameSynth`).
    #[must_use]
    pub fn new(sample_rate: SampleRateHz) -> Self {
        // Shared handles. Cloned into the graph via `var(&…)`; written by
        // `set_param` via `Shared::set`.
        let filter_freq = shared(200.0);
        let filter_q = shared(6.0);
        let noise_gain = shared(0.0);
        let osc_gain = shared(0.0);
        let chord_gain = shared(0.0);
        let master = shared(0.0);
        // Seed the chord at degree 0 major so the graph is well-defined before
        // the first `chord_degree` push.
        let init = chord_frequencies(0.0, true);
        let freq_root = shared(init.root);
        let freq_third = shared(init.third);
        let freq_fifth = shared(init.fifth);
        let freq_sub = shared(init.sub);
        let freq_sub2 = shared(init.sub2);

        // ----- Chord voice (v4 createChord) -----
        //
        // Five oscillators summed at fixed voice gains, then scaled by the
        // smoothed chord master gain. Sines for root/third/fifth, triangles for
        // the two sub-octaves. Each `var(&freq) >> sine()` reads its frequency
        // from the Shared handle so `recompute_chord` retunes the chord live.
        let osc_root = var(&freq_root) >> sine::<f64>();
        let osc_third = var(&freq_third) >> sine::<f64>();
        let osc_fifth = var(&freq_fifth) >> sine::<f64>();
        let osc_sub = var(&freq_sub) >> triangle();
        let osc_sub2 = var(&freq_sub2) >> triangle();
        let chord_mix = osc_root
            + osc_third
            + osc_fifth * CHORD_GAIN_FIFTH
            + osc_sub * CHORD_GAIN_SUB
            + osc_sub2 * CHORD_GAIN_SUB2;
        let chord_voice = chord_mix * (var(&chord_gain) >> follow(PARAM_SMOOTHING_S));

        // ----- Noise voice -----
        //
        // v4's noise is unfiltered white noise straight into the compressor;
        // only its gain is name/velocity-driven. The name-tuned lowpass lives
        // on the osc voice below, not here.
        let noise_voice = white() * (var(&noise_gain) >> follow(PARAM_SMOOTHING_S));

        // ----- "Osc" voice — the v4 DC quirk -----
        //
        // v4 runs a square oscillator at 0 Hz (a constant +1) at gain 0.6
        // through the name-tuned resonant lowpass. The audible signal is the
        // gain *modulation*; the resonant filter rings it. We reproduce the
        // constant with `dc(OSC_VOICE_DC)` (0.6) and feed the SVF cutoff/Q from
        // the smoothed Shared handles:
        //   (dc·gain | cutoff | q) >> lowpass()
        let osc_source = dc(OSC_VOICE_DC) * (var(&osc_gain) >> follow(PARAM_SMOOTHING_S));
        let osc_voice = (osc_source
            | (var(&filter_freq) >> follow(PARAM_SMOOTHING_S))
            | (var(&filter_q) >> follow(PARAM_SMOOTHING_S)))
            >> lowpass::<f64>();

        // ----- Mix + post -----
        //
        // Sum the three voices, soft-limit with a tanh shaper (replacing v4's
        // DynamicsCompressor), then apply the smoothed master gain. `tanh`
        // bounds the output to (-1, 1) regardless of source levels.
        let mix = chord_voice + noise_voice + osc_voice;
        let post = (mix >> shape(Tanh(1.0))) * (var(&master) >> follow(PARAM_SMOOTHING_S));

        let mut graph: Box<dyn AudioUnit> = Box::new(post);
        graph.set_sample_rate(sample_rate);
        graph.allocate();

        // Nyquist-clamp ceiling for this sample rate.
        let max_filter_hz = cutoff_ceiling(sample_rate);

        Self {
            graph,
            filter_freq,
            filter_q,
            noise_gain,
            osc_gain,
            chord_gain,
            master,
            freq_root,
            freq_third,
            freq_fifth,
            freq_sub,
            freq_sub2,
            noise_gain_value: 0.0,
            osc_gain_value: 0.0,
            chord_gain_value: 0.0,
            noise_gain_scale: 0.7,
            has_noise: false,
            is_major: true,
            chord_degree: 0.0,
            density: 1.0,
            chord_energy_scale: 1.0,
            volume_scale: 0.0,
            camera_gain: 0.5,
            max_filter_hz,
        }
    }

    /// Apply a `SetFlameParam` write. Unknown keys are warned and dropped.
    ///
    /// `&mut self` because `morph_energy` reads and rewrites the one-pole
    /// accumulator fields (v4 `updateFromFractalStats`), and `is_major` /
    /// `chord_degree` retune the chord via `Self::recompute_chord`.
    pub fn set_param(&mut self, key: &'static str, value: f32) {
        match key {
            "morph_energy" => self.apply_morph_energy(value),
            "camera_distance" => {
                // v4: `context.gain = 1/(1 + max(0, dist)) + 0.5`.
                self.camera_gain = 1.0 / (1.0 + value.max(0.0)) + 0.5;
                self.recompute_master();
            }
            "volume_scale" => {
                self.volume_scale = value.max(0.0);
                self.recompute_master();
            }
            "duck_pulse" => {
                // Name-change anti-click dip: drop master immediately. The next
                // `camera_distance` push restores it through the follow smoother.
                self.master.set(0.0);
            }
            "filter_freq" => self
                .filter_freq
                .set(value.clamp(MIN_FILTER_HZ, self.max_filter_hz)),
            "filter_q" => self.filter_q.set(value.max(0.1)),
            "noise_scale" => self.noise_gain_scale = value,
            "has_noise" => self.has_noise = value > 0.5,
            "is_major" => {
                self.is_major = value > 0.5;
                self.recompute_chord();
            }
            "chord_degree" => {
                self.chord_degree = value;
                self.recompute_chord();
            }
            "density" => self.density = value,
            "chord_energy" => self.chord_energy_scale = value.max(0.0),
            other => {
                tracing::warn!(key = other, value, "dropping unknown SetFlameParam key");
            }
        }
    }

    /// Pull one mono sample from the graph. Allocation-free.
    #[inline]
    pub fn tick_mono(&mut self) -> f32 {
        self.graph.get_mono()
    }

    /// v4 `updateFromFractalStats`: drive the three voice one-poles from the
    /// per-frame morph energy. Called once per render frame by the coupling
    /// system, so the 0.5 / 0.9 filter constants are frame-tied exactly as v4's.
    fn apply_morph_energy(&mut self, value: f32) {
        // v4 `velocityFactor = min(velocity * noiseGainScale, 0.06)`.
        let vf = (value * self.noise_gain_scale).min(0.06);

        // Noise: `g = g*0.5 + 0.5*(vf * 2/(1+density^2) + 1e-5)` when enabled.
        if self.has_noise {
            let noise_amplitude = 2.0 / (1.0 + self.density * self.density);
            self.noise_gain_value =
                self.noise_gain_value * 0.5 + 0.5 * (vf * noise_amplitude + 1e-5);
        } else {
            self.noise_gain_value = 0.0;
        }

        // Osc: `g = g*0.9 + 0.1*max(0, min(value^2 * 2000, 0.6) - 0.01)`.
        let osc_drive = ((value * value * 2000.0).min(0.6) - 0.01).max(0.0);
        self.osc_gain_value = self.osc_gain_value * 0.9 + 0.1 * osc_drive;

        // Chord: `g = g*0.9 + 0.1*(vf * chord_energy_scale + 1e-4)`.
        // `chord_energy_scale` stands in for v4's `count^2 / 8`.
        self.chord_gain_value =
            self.chord_gain_value * 0.9 + 0.1 * (vf * self.chord_energy_scale + 1e-4);

        self.noise_gain.set(self.noise_gain_value);
        self.osc_gain.set(self.osc_gain_value);
        self.chord_gain.set(self.chord_gain_value);
    }

    /// `master = camera_gain * volume_scale`. The `follow` smoother in the graph
    /// ramps to the new target; a preceding `duck_pulse` zeroes the raw value so
    /// the next master push climbs back in from silence.
    fn recompute_master(&self) {
        self.master.set(self.camera_gain * self.volume_scale);
    }

    /// Recompute and push the five chord frequencies from the current
    /// `chord_degree` + `is_major`.
    fn recompute_chord(&self) {
        let f = chord_frequencies(self.chord_degree, self.is_major);
        self.freq_root.set(f.root);
        self.freq_third.set(f.third);
        self.freq_fifth.set(f.fifth);
        self.freq_sub.set(f.sub);
        self.freq_sub2.set(f.sub2);
    }

    /// Current noise-gain one-pole accumulator. Test-only accessor for the
    /// morph-energy rise/fall assertions.
    #[cfg(test)]
    fn debug_noise_gain(&self) -> f32 {
        self.noise_gain_value
    }
}

impl core::fmt::Debug for FlameSynth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FlameSynth")
            .field("filter_freq", &self.filter_freq.value())
            .field("filter_q", &self.filter_q.value())
            .field("noise_gain", &self.noise_gain_value)
            .field("osc_gain", &self.osc_gain_value)
            .field("chord_gain", &self.chord_gain_value)
            .field("master", &self.master.value())
            .field("is_major", &self.is_major)
            .field("chord_degree", &self.chord_degree)
            .field("has_noise", &self.has_noise)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    #[test]
    fn synth_ticks_finite_audio() {
        let mut synth = FlameSynth::new(48_000.0);
        // Raise the gains so the graph produces signal.
        synth.set_param("has_noise", 1.0);
        synth.set_param("noise_scale", 1.0);
        synth.set_param("density", 1.5);
        synth.set_param("camera_distance", 0.78);
        synth.set_param("volume_scale", 1.0);
        for _ in 0..60 {
            synth.set_param("morph_energy", 0.05);
        }
        let mut peak = 0.0_f32;
        for _ in 0..4800 {
            let s = synth.tick_mono();
            assert!(s.is_finite());
            peak = peak.max(s.abs());
        }
        assert!(peak > 0.0, "audible output after morph energy");
        assert!(peak <= 1.0, "tanh/limiter bounds the mix");
    }

    /// Chord frequency math golden: degree 0, major -> root 120 Hz, "third"
    /// = scale index 3 = 5 semitones -> 120 * 2^(5/12), fifth = index 5 = 9
    /// semitones -> 120 * 2^(9/12); subs at /2 and /4.
    #[test]
    fn chord_frequencies_match_v4_scale_math() {
        let f = chord_frequencies(0.0, true);
        assert!((f.root - 120.0).abs() < 1e-3);
        assert!((f.third - 120.0 * 2_f32.powf(5.0 / 12.0)).abs() < 1e-2);
        assert!((f.fifth - 120.0 * 2_f32.powf(9.0 / 12.0)).abs() < 1e-2);
        assert!((f.sub - f.root / 2.0).abs() < 1e-4);
        assert!((f.sub2 - f.root / 4.0).abs() < 1e-4);
        // Minor third: index 3 in MINOR = 5 semitones too, so probe degree 1
        // where major/minor diverge (MAJOR[4]=7 vs MINOR[4]=7 — use degree 2:
        // third index 5 -> MAJOR 9 vs MINOR 8).
        let maj = chord_frequencies(2.0, true);
        let min = chord_frequencies(2.0, false);
        assert!(maj.third > min.third);
    }

    /// The morph-energy one-poles rise monotonically under sustained energy
    /// and decay when it stops (v4's 0.5/0.9 accumulators).
    #[test]
    fn morph_energy_gains_rise_and_fall() {
        let mut synth = FlameSynth::new(48_000.0);
        synth.set_param("has_noise", 1.0);
        synth.set_param("noise_scale", 1.0);
        synth.set_param("density", 1.5);
        let mut last = 0.0;
        for _ in 0..30 {
            synth.set_param("morph_energy", 0.05);
            assert!(synth.debug_noise_gain() >= last);
            last = synth.debug_noise_gain();
        }
        let peak = last;
        for _ in 0..60 {
            synth.set_param("morph_energy", 0.0);
        }
        assert!(synth.debug_noise_gain() < peak);
    }

    #[test]
    fn unknown_key_is_dropped_without_panic() {
        let mut synth = FlameSynth::new(48_000.0);
        synth.set_param("definitely_not_a_key", 1.0);
    }
}
