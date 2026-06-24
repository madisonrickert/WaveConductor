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
        Self {
            pcm,
            channels,
            frames,
        }
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
        Self {
            sample: None,
            playhead: 0.0,
        }
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
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::as_conversions,
        reason = "DSP math: frames is a sample count (<<2^52 for audio), pos is \
                  non-negative after rem_euclid, frac in [0,1) — all casts are \
                  value-safe in this domain"
    )]
    pub fn mix_frame(
        &mut self,
        sample: Option<&SampleData>,
        frame: &mut [f32],
        volume: f32,
        rate: f64,
    ) {
        let Some(s) = sample else { return };
        if s.frames == 0 {
            return;
        }
        let frames = s.frames;
        // Wrap the playhead into [0, frames) before reading. `rem_euclid`
        // handles negative positions if rate is ever reversed.
        // `frames` is a sample count (<<2^52), so f64 precision is exact here.
        let pos = self.playhead.rem_euclid(frames as f64);
        // Integer and fractional parts of the current position.
        let i0 = pos.floor() as usize % frames;
        // i1 wraps at the sample boundary for the final frame.
        let i1 = (i0 + 1) % frames;
        // `frac` is exactly 0.0 when `pos` is an integer (e.g. rate 1.0 with
        // integer playhead), so `lerp(a, b, 0) == a` — bit-exact reads.
        let frac = (pos - pos.floor()) as f32;
        // Stack-allocated scratch; keeps mix_frame allocation-free on the audio
        // thread (MAX_FRAME_CHANNELS covers all cpal mono/stereo configurations).
        let mut a = [0.0_f32; MAX_FRAME_CHANNELS];
        let mut b = [0.0_f32; MAX_FRAME_CHANNELS];
        let ch = frame.len().min(MAX_FRAME_CHANNELS);
        s.read_frame(i0, &mut a[..ch]);
        s.read_frame(i1, &mut b[..ch]);
        for (slot, (x, y)) in frame.iter_mut().zip(a[..ch].iter().zip(b[..ch].iter())) {
            // lerp: a + (b - a) * frac
            *slot += (x + (y - x) * frac) * volume;
        }
        // Advance; the next call's `rem_euclid` handles wrapping.
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
        Self {
            sample: None,
            playhead: 0.0,
            active: false,
        }
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
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::as_conversions,
        reason = "DSP math: playhead is non-negative and < frames (integer frames, \
                  not fractional for one-shots) — cast is value-safe"
    )]
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
        // Stack-allocated scratch; same allocation-free guarantee as LoopVoice.
        let mut a = [0.0_f32; MAX_FRAME_CHANNELS];
        let ch = frame.len().min(MAX_FRAME_CHANNELS);
        s.read_frame(idx, &mut a[..ch]);
        for (slot, x) in frame.iter_mut().zip(a[..ch].iter()) {
            *slot += x * volume;
        }
        // Rate is always 1.0 for one-shots.
        self.playhead += 1.0;
    }
}

/// Upper bound on output channels we mix per frame. cpal output is mono or
/// stereo in practice; a fixed stack array keeps `mix_frame` allocation-free.
const MAX_FRAME_CHANNELS: usize = 8;

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "tests assert exact bit values on integer ramp data; casts are from \
              small integers (0..100) that are exactly representable as f32/f64"
)]
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
        let bank = SampleBank::from_samples(vec![("a", ramp(4, 1)), ("b", ramp(8, 1))]);
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
    #[allow(unused_assignments)] // `out` is initialized outside the loop then overwritten inside; that's intentional
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
