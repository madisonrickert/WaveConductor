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

#[cfg(test)]
#[path = "analysis_tests.rs"]
mod tests;
