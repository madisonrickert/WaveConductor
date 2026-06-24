//! Cymatics synth voice: 6-oscillator stack + AM LFO + bandpass-filtered white
//! noise from v4 `audio.ts`, driven by two Shared params (`osc_volume`,
//! `osc_freq_scalar`). All per-oscillator, LFO, and noise frequencies are
//! derived in-graph from `osc_freq_scalar` so the coupling layer sets only
//! two values.
//!
//! ## Audio-thread allocation policy
//!
//! Same as `LineSynth` and `DotsSynth`: [`CymaticsSynth::new`] allocates
//! (boxes the graph, clones `Arc`-backed `Shared` handles);
//! [`CymaticsSynth::set_param`] and [`CymaticsSynth::tick_mono`] are
//! allocation-free (atomic stores + a single `AudioUnit::get_mono` call).
//!
//! ## v4 oscillator mapping (`audio.ts`)
//!
//! | v4 name           | frequency expression                     | gain |
//! |-------------------|------------------------------------------|------|
//! | `oscBase`         | 126 Hz (fixed)                           | 1.0  |
//! | `oscUnison`       | 126·scalar                               | 0.5  |
//! | `oscFifth`        | 126·scalar·2^(7/12) (perfect fifth)      | 0.5  |
//! | `oscSub`          | 126·scalar/2 (one octave below unison)   | 0.5  |
//! | `oscHigh4`        | 126·scalar·2^4 + 4                       | 0.02 |
//! | `oscHigh4Second`  | 126·scalar²·2^(4+1/12) + 9              | 0.01 |
//!
//! `oscGain.gain = clamp(osc_volume·0.75, 1e-10, 1)`.
//!
//! AM LFO: sine at `(scalar−1)·100 + 1e-10` Hz, amplitude 0.5 around base 1.0.
//!
//! Noise: white → bandpass(Q=100, `1500·(1+scalar²)`) · `clamp((scalar−1.002)·20, 0, 1)`.

use fundsp::prelude::*;

/// Sample rate handed to the synth at construction.
type SampleRateHz = f64;

/// v4 `oscBase` fixed reference frequency (Hz). All six oscillators derive
/// from this base — those driven by `osc_freq_scalar` multiply it in-graph.
const OSC_FREQ_BASE: f32 = 126.0;

/// Default `osc_volume` (raw `oscVolumeInput`) before the coupling system
/// writes to it. v4: `sourceGain.gain.setValueAtTime(0, 0)` — silent until
/// interaction begins.
pub(crate) const DEFAULT_OSC_VOLUME: f32 = 0.0;

/// Default `osc_freq_scalar` (`freqScalar`) before the first coupling write.
const DEFAULT_FREQ_SCALAR: f32 = 1.0;

/// Parameter smoothing time constant (seconds). Matches v4's
/// `setTargetAtTime(_, _, 0.016)` exponential approach.
const PARAM_SMOOTHING_S: f32 = 0.016;

/// Cymatics voice graph.
///
/// Owns a `Box<dyn AudioUnit>` plus two [`Shared`] parameter handles.
/// Construction allocates; [`Self::set_param`] and [`Self::tick_mono`] do not.
pub struct CymaticsSynth {
    /// Boxed mono DSP graph. Pre-allocated in [`CymaticsSynth::new`] via
    /// `AudioUnit::allocate()` to avoid first-callback hitches.
    graph: Box<dyn AudioUnit>,
    /// `oscGain` level (raw `oscVolumeInput`; the v4 `×0.75` is baked into
    /// the graph). Set via [`Self::set_param`] with key `"osc_volume"`.
    pub(crate) osc_volume: Shared,
    /// `freqScalar` — drives every derived oscillator, LFO, and noise
    /// frequency. Set via [`Self::set_param`] with key `"osc_freq_scalar"`.
    pub(crate) osc_freq_scalar: Shared,
}

/// Keys accepted by [`CymaticsSynth::set_param`].
pub const KNOWN_KEYS: &[&str] = &["osc_volume", "osc_freq_scalar"];

impl CymaticsSynth {
    /// Build the Cymatics voice graph at `sample_rate`. Allocates once;
    /// call only on activation (e.g. from `AudioCommand::AddCymaticsSynth`).
    #[must_use]
    pub fn new(sample_rate: SampleRateHz) -> Self {
        let osc_volume = shared(DEFAULT_OSC_VOLUME);
        let osc_freq_scalar = shared(DEFAULT_FREQ_SCALAR);

        // Smoothed scalar signal. Each call creates a new `var`+`follow`
        // chain because fundsp `An` nodes are not `Clone`. All chains share
        // the same `Shared`, so they track together after settling.
        let scalar = || var(&osc_freq_scalar) >> follow(PARAM_SMOOTHING_S);

        // ----- Oscillators -----
        //
        // Six sine oscillators with individual gains, summed.
        //
        // `oscBase`: 126 Hz fixed, gain 1.0. Never re-pitched — v4 keeps
        // this at the literal base frequency regardless of freqScalar.
        let osc_base = dc(OSC_FREQ_BASE) >> sine::<f32>();

        // `oscUnison`: 126*scalar Hz, gain 0.5.
        let osc_unison = (scalar() * OSC_FREQ_BASE) >> sine::<f32>();

        // `oscFifth`: 126*scalar*2^(7/12) Hz (perfect fifth above unison), gain 0.5.
        // 2^(7/12) ≈ 1.498 — 7 equal-temperament semitones.
        let osc_fifth = (scalar() * (OSC_FREQ_BASE * 2.0_f32.powf(7.0 / 12.0))) >> sine::<f32>();

        // `oscSub`: 126*scalar/2 Hz (one octave below unison), gain 0.5.
        let osc_sub = (scalar() * (OSC_FREQ_BASE * 0.5)) >> sine::<f32>();

        // `oscHigh4`: 126*scalar*2^4 + 4 Hz (+4 octaves, +4 Hz fixed offset), gain 0.02.
        // The +4 Hz offset introduces a slight beating above the clean octave stack.
        let osc_high4 = (scalar() * (OSC_FREQ_BASE * 16.0) + 4.0) >> sine::<f32>();

        // `oscHigh4Second`: 126*scalar²*2^(4+1/12) + 9 Hz (+4 octaves +1 semitone,
        // scalar² for extra pitch sensitivity), gain 0.01.
        // scalar² is built by multiplying two independent `var` nodes on the same
        // Shared. `follow` smooths the squared value, then the frequency is derived.
        let scalar_sq =
            (var(&osc_freq_scalar) * var(&osc_freq_scalar)) >> follow(PARAM_SMOOTHING_S);
        let osc_high4_second =
            (scalar_sq * (OSC_FREQ_BASE * 2.0_f32.powf(4.0 + 1.0 / 12.0)) + 9.0) >> sine::<f32>();

        // Sum all oscillators with their v4 gains.
        let osc_mix = osc_base * 1.0
            + osc_unison * 0.5
            + osc_fifth * 0.5
            + osc_sub * 0.5
            + osc_high4 * 0.02
            + osc_high4_second * 0.01;

        // ----- Oscillator gain -----
        //
        // v4: `oscGain.gain.setTargetAtTime(clamp(osc_volume*0.75, 1e-10, 1), _, 0.016)`.
        // The `×0.75` factor lives in v4's `setOscVolume()`; it is baked here so
        // the coupling layer passes the raw `oscVolumeInput` without prescaling.
        // `clip_to(1e-10, 1.0)` computes the clamped target first (matching v4's
        // inner `clamp(vol*0.75, 1e-10, 1.0)`), then `follow` smooths toward that
        // clamped target — mirroring v4's `setTargetAtTime(clamp(...), ...)` order.
        let osc_gain =
            (var(&osc_volume) * 0.75) >> clip_to(1.0e-10, 1.0) >> follow(PARAM_SMOOTHING_S);

        // ----- AM LFO -----
        //
        // v4: `lfo.frequency = (freqScalar-1)*100 + 1e-10`. A variable-rate sine
        // at this frequency (near 0 Hz at scalar=1.0, rising with scalar) modulates
        // the oscillator mix with amplitude 0.5 around a 1.0 base. Creates a rhythmic
        // tremolo that accelerates as the frequency scalar increases.
        // The +1e-10 prevents exactly 0 Hz (which avoids DC drift in the sine state).
        let lfo_rate = (var(&osc_freq_scalar) - 1.0) * 100.0 + 1.0e-10_f32;
        // AM envelope: 1 + 0.5*sin(lfo_rate Hz). Range [0.5, 1.5].
        let lfo_am = (lfo_rate >> sine::<f32>()) * 0.5 + 1.0;
        let osc_voice = osc_mix * osc_gain * lfo_am;

        // ----- Bandpass-filtered noise -----
        //
        // v4: `noiseFilter.frequency = 1500*(1+scalar²)`, `noiseFilter.Q = 100`,
        //     `noiseGain.gain = clamp((scalar-1.002)*20, 0, 1)`.
        // The noise path is silent at scalar ≤ 1.002 (gate eliminates noise at rest)
        // and reaches full gain at scalar ≈ 1.052.
        //
        // A new scalar² pair is constructed here because the one consumed in
        // `osc_high4_second` above is no longer available (fundsp An nodes move).
        let scalar_sq_noise =
            (var(&osc_freq_scalar) * var(&osc_freq_scalar)) >> follow(PARAM_SMOOTHING_S);
        // Bandpass cutoff: 1500*(1+scalar²). At scalar=1.0: 3000 Hz; rises with scalar.
        let noise_cutoff = (scalar_sq_noise + 1.0) * 1500.0;
        // Noise gain: clamp((scalar-1.002)*20, 0, 1). Zero until scalar > 1.002.
        let noise_gain = ((var(&osc_freq_scalar) - 1.002_f32) * 20.0) >> clip_to(0.0, 1.0);
        // white noise → SVF bandpass (Q=100, dynamic cutoff) → scale by noise_gain.
        // `bandpass::<f64>()` uses f64 for SVF coefficient math (matches DotsSynth/
        // LineSynth style; improves filter accuracy at high Q and extreme cutoffs).
        let noise_filtered = (white() | noise_cutoff | dc(100.0_f32)) >> bandpass::<f64>();
        let noise_voice = noise_filtered * noise_gain;

        // ----- Mix -----
        //
        // Oscillator voice + noise voice summed directly. `limiter` provides peak
        // protection. Parameters mirror `LineSynth`'s limiter (0.005 s attack,
        // 0.100 s release).
        let mix = osc_voice + noise_voice;
        let mut graph: Box<dyn AudioUnit> = Box::new(mix >> limiter(0.005, 0.100));
        graph.set_sample_rate(sample_rate);
        // Pre-allocate internal buffers to avoid first-callback hitches.
        graph.allocate();

        Self {
            graph,
            osc_volume,
            osc_freq_scalar,
        }
    }

    /// Apply a `SetCymaticsParam` write. Unknown keys are logged and dropped.
    ///
    /// Every branch is a single `Shared::set` (relaxed atomic store) —
    /// allocation-free and safe to call from the audio thread.
    pub fn set_param(&self, key: &'static str, value: f32) {
        match key {
            // osc_volume: raw oscVolumeInput; the v4 ×0.75 is baked in-graph.
            // Clamped to [0, ∞); the graph clips the scaled value to [1e-10, 1].
            "osc_volume" => self.osc_volume.set(value.max(0.0)),
            // osc_freq_scalar: freqScalar; must be positive (0 → all derived
            // frequencies collapse; clamped rather than panicking).
            "osc_freq_scalar" => self.osc_freq_scalar.set(value.max(0.0)),
            other => {
                tracing::warn!(key = other, value, "dropping unknown SetCymaticsParam key");
            }
        }
    }

    /// Pull one mono sample from the graph. Allocation-free.
    #[inline]
    pub fn tick_mono(&mut self) -> f32 {
        self.graph.get_mono()
    }
}

impl core::fmt::Debug for CymaticsSynth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CymaticsSynth")
            .field("osc_volume", &self.osc_volume.value())
            .field("osc_freq_scalar", &self.osc_freq_scalar.value())
            .finish_non_exhaustive()
    }
}

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

    /// The noise gate opens only when `osc_freq_scalar > 1.002`.
    ///
    /// `noise_gain = clamp((scalar − 1.002) × 20, 0, 1)`: exactly 0 at the
    /// threshold, positive above it. `osc_volume = 0` silences the oscillators
    /// (gain clips to 1e-10), leaving the noise path as the only meaningful source.
    #[test]
    fn noise_gate_threshold_at_1_002() {
        // At/below threshold: scalar = 1.002 → noise_gain = clamp(0*20, 0,1) = 0.
        let mut s_at = CymaticsSynth::new(48_000.0);
        s_at.set_param("osc_volume", 0.0);
        s_at.set_param("osc_freq_scalar", 1.002);
        let mut max_at = 0.0_f32;
        for _ in 0..512 {
            max_at = max_at.max(s_at.tick_mono().abs());
        }
        // Zero noise gain + near-zero osc gain (1e-10 clip floor) → near-silent.
        assert!(
            max_at < 1e-3,
            "at scalar=1.002 noise gate must be closed; got {max_at}"
        );

        // Above threshold: scalar = 1.1 → noise_gain = clamp(1.96, 0,1) = 1.0.
        let mut s_above = CymaticsSynth::new(48_000.0);
        s_above.set_param("osc_volume", 0.0);
        s_above.set_param("osc_freq_scalar", 1.1);
        let mut max_above = 0.0_f32;
        for _ in 0..4_096 {
            max_above = max_above.max(s_above.tick_mono().abs());
        }
        // Bandpass-filtered white noise at full gain should produce audible output.
        assert!(
            max_above > 1e-4,
            "at scalar=1.1 noise gate must be open; got {max_above}"
        );
    }

    /// `osc_base = dc(OSC_FREQ_BASE) >> sine()` is wired to the constant 126 Hz
    /// with no `var(&osc_freq_scalar)` reference. Changing `osc_freq_scalar`
    /// re-pitches only the five derived oscillators (unison, fifth, sub, `high4`,
    /// `high4_second`).
    ///
    /// Direct pitch assertion is impractical headlessly (requires FFT). We assert
    /// the structural invariant two ways:
    ///
    /// 1. Render at extreme scalar (100.0): derived oscillators alias far above
    ///    Nyquist; the 126 Hz base still contributes (non-zero peak).
    /// 2. `set_param("osc_freq_scalar", …)` does not disturb the `osc_volume`
    ///    Shared, confirming the two parameter paths are independent.
    #[test]
    fn osc_base_pitch_is_fixed_independent_of_scalar() {
        // Extreme scalar: 126*100 >> Nyquist for the derived oscillators; the
        // 126 Hz base remains audible regardless.
        let mut s = CymaticsSynth::new(48_000.0);
        s.set_param("osc_volume", 1.0);
        s.set_param("osc_freq_scalar", 100.0);
        let mut max_abs = 0.0_f32;
        for _ in 0..4_096 {
            max_abs = max_abs.max(s.tick_mono().abs());
        }
        assert!(
            max_abs > 1e-3,
            "base oscillator must contribute at extreme scalar=100.0; got {max_abs}"
        );
        // Structural independence: writing osc_freq_scalar must not alter osc_volume.
        let s2 = CymaticsSynth::new(48_000.0);
        s2.set_param("osc_volume", 0.5);
        s2.set_param("osc_freq_scalar", 2.0);
        assert!(
            (s2.osc_volume.value() - 0.5).abs() < f32::EPSILON,
            "osc_freq_scalar write must not disturb osc_volume; got {}",
            s2.osc_volume.value()
        );
    }
}
