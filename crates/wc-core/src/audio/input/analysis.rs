//! Ring drain + DSP analysis for the audio-input path.
//!
//! Everything in this module runs on the **Bevy main thread**, once per
//! frame (`PreUpdate`), downstream of the lock-free ring the cpal input
//! callback fills (see `super::capture`). The core is `AnalysisEngine`
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
            hann: hann_window(),
            magnitudes: vec![0.0; SPECTRUM_LEN],
            band_bins: band_bins(sample_rate),
            bands: [0.0; AUDIO_BAND_COUNT],
            prev_magnitudes: vec![0.0; SPECTRUM_LEN],
            flux_mean: 0.0,
            // Start "ready": the first onset may immediately be a beat.
            seconds_since_beat: MIN_BEAT_INTERVAL_S,
            beat_confidence: 0.0,
            beats: 0,
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
        self.peak = ((window_peak * gain).min(1.0)).max(self.peak * (-dt / PEAK_DECAY_TAU_S).exp());

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
        for (m, s) in self.magnitudes.iter_mut().zip(spectrum.iter()).skip(1) {
            *m = s.norm() * SPECTRUM_NORM;
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

        AudioAnalysis {
            rms: self.smoothed_rms,
            gain,
            bands: self.bands,
            onset,
            beat_confidence: self.beat_confidence,
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

    /// Total debounced beats detected since construction/reset
    /// (test/diagnostic surface).
    pub fn beat_count(&self) -> u64 {
        self.beats
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

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
