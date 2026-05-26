//! `DspHost` — the audio-thread DSP graph.
//!
//! Owns the master volume, mute state, and any active per-sketch voice
//! graphs; processes incoming [`AudioCommand`]s; renders samples into the
//! cpal output buffer. Fully unit-testable without any audio hardware:
//! construct, apply commands, render into a `Vec<f32>`, assert.
//!
//! ## Real-time invariants
//!
//! - `render` is allocation-free.
//! - `apply` is allocation-free **except** for `AddLineSynth`, which boxes a
//!   new voice graph exactly once per sketch activation. This is a one-shot
//!   cost on the audio thread that callers tolerate at sketch boundaries.
//! - `RemoveLineSynth` drops the boxed graph on the audio thread; the
//!   deallocation is bounded (a handful of `Arc` and `Box` frees, no
//!   recursive structure that could surprise us).
//!
//! Plan 9 Phase A wires the Line synth voice graph; Phase B adds background
//! sample mixing; Phase C/D wire reactivity from `ParticleStats`.
//!
//! ## Background sample mixing (Phase B)
//!
//! The constructor accepts a pre-decoded, pre-resampled, **interleaved**
//! `Vec<f32>` of the same channel count as the cpal output. The audio
//! thread loops the buffer via a `playhead: usize` frame index that wraps
//! at the buffer's frame count. The mix is:
//!
//! ```text
//! out = gain · clamp(synth + background · background_volume, -1.0, 1.0)
//! ```
//!
//! The inner clamp prevents output overflow when both sources peak
//! simultaneously; the outer master `gain` (volume × !muted) is applied
//! after so that muting still produces silence regardless of source levels.
//!
//! `background_volume` is set via `AudioCommand::SetLineParam { key:
//! "background_volume", value }`. The default on construction is `1.0`
//! to match v4's `volume: 1.0` on its `AudioClip`.

use fundsp::shared::Shared;

use super::command::AudioCommand;
use super::line_synth::LineSynth;

/// Default amplitude scalar applied to the background sample. Matches v4
/// `AudioClip { volume: 1.0 }`. Coupled to the same `Shared<f32>` handle
/// that `SetLineParam { key: "background_volume" }` writes.
const DEFAULT_BACKGROUND_VOLUME: f32 = 1.0;

/// `SetLineParam` key that routes to the background-sample amplitude
/// scalar. Kept here rather than in `LineSynth::KNOWN_KEYS` because
/// background volume is a host-level mixer parameter, not a synth-graph
/// parameter — the host owns the sample buffer and the playhead.
const BACKGROUND_VOLUME_KEY: &str = "background_volume";

/// DSP host owned by the cpal audio thread.
///
/// All hot-path state is either plain `Copy`/`f32` or an `Option<Box<…>>`
/// whose presence is checked once per buffer.
///
/// `Debug` is hand-rolled because `fundsp::Shared` does not implement it;
/// the formatter prints the loaded value via `Shared::value()`.
pub struct DspHost {
    /// Output sample rate in Hz. Forwarded to `LineSynth::new` when a synth
    /// is added so its internal `set_sample_rate` is correct.
    sample_rate: u32,
    /// Output channel count. Used by `render` to splat the mono synth
    /// output across the interleaved buffer.
    channels: u16,
    volume: f32,
    muted: bool,
    /// Active Line voice graph, if any. `None` means the sketch is not
    /// loaded and the synth contributes nothing to the output mix.
    line_synth: Option<LineSynth>,
    /// Pre-decoded, pre-resampled interleaved PCM for the background
    /// sample. Layout is `[L, R, L, R, ...]` for stereo, or `[M, M, ...]`
    /// for mono — channel count always equals `self.channels`. Empty when
    /// no background asset is available (file missing, decode failed); the
    /// render path treats an empty buffer as silence.
    background_pcm: Vec<f32>,
    /// Current playhead in **frames** (not samples). Wraps at
    /// `background_pcm.len() / channels` each buffer to loop indefinitely.
    /// Written once per buffer in [`Self::render`].
    playhead: usize,
    /// Amplitude scalar applied to the background sample before it joins
    /// the synth mix. A `Shared<f32>` so future smoothing (`follow`) can
    /// be added cleanly; for now the audio thread reads it directly each
    /// buffer via `Shared::value()`. The value is an atomic load, no
    /// allocation.
    background_volume: Shared,
}

impl DspHost {
    /// Construct a default-silent host for the given output format.
    ///
    /// `background_pcm` is the pre-decoded, pre-resampled interleaved PCM
    /// buffer the audio thread will loop. Its channel count must equal
    /// `channels`. Pass an empty `Vec` to disable the background mix.
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16, background_pcm: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            volume: 1.0,
            muted: false,
            line_synth: None,
            background_pcm,
            playhead: 0,
            background_volume: Shared::new(DEFAULT_BACKGROUND_VOLUME),
        }
    }

    /// Apply a command. Clamps and validates values that the type system
    /// can't constrain (e.g., `SetMasterVolume` outside `[0, 1]`).
    ///
    /// `AddLineSynth` is idempotent (a second add while active is a no-op).
    /// `RemoveLineSynth` is idempotent (a remove with no active synth is a
    /// no-op). `SetLineParam` with a synth not yet active is dropped via a
    /// `tracing::warn!`; the audio thread never panics on stale params.
    pub fn apply(&mut self, command: AudioCommand) {
        match command {
            AudioCommand::SetMasterVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
            }
            AudioCommand::SetMuted(m) => {
                self.muted = m;
            }
            AudioCommand::AddLineSynth => {
                if self.line_synth.is_none() {
                    self.line_synth = Some(LineSynth::new(f64::from(self.sample_rate)));
                }
            }
            AudioCommand::RemoveLineSynth => {
                self.line_synth = None;
            }
            AudioCommand::SetLineParam { key, value } => {
                // `background_volume` is a host-level mixer parameter
                // (it scales the looped PCM buffer this struct owns) and
                // therefore stays valid regardless of whether the synth
                // is active. Handle it here before delegating the rest to
                // the synth's per-graph parameter table.
                if key == BACKGROUND_VOLUME_KEY {
                    self.background_volume.set(value.max(0.0));
                } else if let Some(synth) = &self.line_synth {
                    synth.set_param(key, value);
                } else {
                    tracing::warn!(
                        key,
                        value,
                        "SetLineParam received with no active LineSynth; dropping"
                    );
                }
            }
        }
    }

    /// Current master volume in `[0.0, 1.0]`. Cached for status reporting.
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Current mute state.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// True if a Line voice graph is currently active.
    #[must_use]
    pub fn line_synth_active(&self) -> bool {
        self.line_synth.is_some()
    }

    /// Current background-sample amplitude scalar. Reflects the most
    /// recent `SetLineParam { key: "background_volume" }` write the audio
    /// thread has observed.
    #[must_use]
    pub fn background_volume(&self) -> f32 {
        self.background_volume.value()
    }

    /// True if a non-empty background PCM buffer is loaded. When false,
    /// the background mix contributes zero regardless of volume.
    #[must_use]
    pub fn has_background(&self) -> bool {
        !self.background_pcm.is_empty()
    }

    /// Render samples into `output`.
    ///
    /// `output` is a flat slice in cpal's interleaved layout: `[L, R, L, R, …]`
    /// for stereo. The buffer length is divisible by `channels`.
    ///
    /// Mixing per frame:
    /// 1. Pull one mono sample from the Line synth (zero if not active).
    /// 2. Pull one interleaved frame from the background PCM at `playhead`
    ///    (zeros if the buffer is empty), scale by `background_volume`.
    /// 3. Sum synth (broadcast across channels) with the per-channel
    ///    background sample, clamp to `[-1.0, 1.0]` to avoid output
    ///    overflow when both sources peak simultaneously.
    /// 4. Multiply by the master `gain` (`muted ? 0 : volume`).
    /// 5. Write into the output frame's channel slots.
    ///
    /// The playhead advances by one frame per output frame and wraps at
    /// the buffer's frame count for indefinite looping. When the buffer
    /// is empty, `playhead` is left at 0.
    pub fn render(&mut self, output: &mut [f32]) {
        // Compute the effective gain once per buffer; both volume and mute
        // are buffer-level (no per-sample envelope yet). The synth applies
        // its own internal volume scaling via the `volume` Shared param;
        // the master gain here is the user-facing volume knob plus mute.
        let gain = if self.muted { 0.0 } else { self.volume };
        // `u16 → usize` is infallible on every target we support; using
        // `usize::from` keeps the workspace `as_conversions` lint happy
        // without a fallible `try_from`.
        let channels = usize::from(self.channels.max(1));
        let bg_frames = self.background_pcm.len() / channels;
        // Atomic load once per buffer; the smoothing pass that prevents
        // audible zipper noise on rapid changes is deferred to the
        // reactivity-coupling layer (it sets this value at most once per
        // visual frame, ~16ms).
        let bg_volume = self.background_volume.value();

        for frame in output.chunks_mut(channels) {
            // Synth contribution (broadcast across all channels).
            let synth_sample = match self.line_synth.as_mut() {
                Some(synth) => synth.tick_mono(),
                None => 0.0,
            };

            // Background contribution, per channel. Read the current
            // frame at `playhead` then advance + wrap.
            if bg_frames > 0 {
                let base = self.playhead * channels;
                for (i, slot) in frame.iter_mut().enumerate() {
                    let bg = self.background_pcm[base + i] * bg_volume;
                    // Inner clamp guards against summed-peak clipping;
                    // outer `gain` multiplier never increases magnitude
                    // since it is in `[0, 1]`.
                    let mixed = (synth_sample + bg).clamp(-1.0, 1.0);
                    *slot = mixed * gain;
                }
                self.playhead += 1;
                if self.playhead >= bg_frames {
                    self.playhead = 0;
                }
            } else {
                // No background; just splat the (clamped) synth across
                // channels. The clamp here is paranoid — the synth's
                // internal `limiter` should keep it inside [-1, 1] — but
                // costs nothing and matches the with-background path.
                let mixed = synth_sample.clamp(-1.0, 1.0) * gain;
                for slot in frame.iter_mut() {
                    *slot = mixed;
                }
            }
        }
    }
}

impl core::fmt::Debug for DspHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DspHost")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("volume", &self.volume)
            .field("muted", &self.muted)
            .field("line_synth_active", &self.line_synth.is_some())
            .field(
                "background_frames",
                &(self.background_pcm.len() / usize::from(self.channels.max(1))),
            )
            .field("playhead", &self.playhead)
            .field("background_volume", &self.background_volume.value())
            .finish()
    }
}

#[cfg(test)]
#[path = "dsp_tests.rs"]
mod tests;
