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
#[allow(
    clippy::float_cmp,
    reason = "EPSILON comparisons are appropriate for test assertions on clean f32 values"
)]
mod tests {
    use super::*;

    #[test]
    fn default_host_renders_silence() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        let mut buffer = vec![1.0_f32; 256];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn set_master_volume_clamps_range() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::SetMasterVolume(1.5));
        assert!((host.volume() - 1.0).abs() < f32::EPSILON);
        host.apply(AudioCommand::SetMasterVolume(-0.2));
        assert!(host.volume().abs() < f32::EPSILON);
        host.apply(AudioCommand::SetMasterVolume(0.5));
        assert!((host.volume() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn set_muted_updates_state() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        assert!(!host.muted());
        host.apply(AudioCommand::SetMuted(true));
        assert!(host.muted());
        host.apply(AudioCommand::SetMuted(false));
        assert!(!host.muted());
    }

    #[test]
    fn muted_render_outputs_zero_even_when_volume_high() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::SetMasterVolume(1.0));
        host.apply(AudioCommand::SetMuted(true));
        // Activate the synth and crank its internal volume up too: muted
        // should still force silence.
        host.apply(AudioCommand::AddLineSynth);
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        host.apply(AudioCommand::SetLineParam {
            key: "bandpass_freq",
            value: 320.0,
        });
        let mut buffer = vec![0.5_f32; 64];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn add_line_synth_activates() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::AddLineSynth);
        assert!(host.line_synth_active());
        // Crank volume so the smoothed source-gain ramps in.
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        host.apply(AudioCommand::SetLineParam {
            key: "bandpass_freq",
            value: 320.0,
        });
        host.apply(AudioCommand::SetLineParam {
            key: "noise_freq",
            value: 800.0,
        });
        // Render enough samples for the `follow(0.016)` smoothers to ramp.
        // 48k * 0.05 = 2400 samples ≈ 50 ms, well past the 16 ms time
        // constant.
        let mut buffer = vec![0.0_f32; 2400 * 2];
        host.render(&mut buffer);
        let max_abs = buffer.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
        assert!(
            max_abs > 0.0001,
            "expected audible output after AddLineSynth + volume ramp, max_abs = {max_abs}"
        );
    }

    #[test]
    fn remove_line_synth_silences() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::AddLineSynth);
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        // Warm up the synth so smoothers are at steady-state.
        let mut warm = vec![0.0_f32; 2400 * 2];
        host.render(&mut warm);
        host.apply(AudioCommand::RemoveLineSynth);
        assert!(!host.line_synth_active());
        let mut buffer = vec![1.0_f32; 256];
        host.render(&mut buffer);
        assert!(
            buffer.iter().all(|s| s.abs() < f32::EPSILON),
            "expected silence after RemoveLineSynth"
        );
    }

    #[test]
    fn unknown_param_key_drops_gracefully() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::AddLineSynth);
        // Apply an unknown key; should not panic. Then render to confirm
        // the host is still operational.
        host.apply(AudioCommand::SetLineParam {
            key: "no_such_key",
            value: 1.0,
        });
        let mut buffer = vec![0.0_f32; 128];
        host.render(&mut buffer);
    }

    #[test]
    fn set_line_param_with_no_synth_does_not_panic() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        // No synth active; SetLineParam should warn-and-drop, never panic.
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        assert!(!host.line_synth_active());
    }

    #[test]
    fn add_line_synth_is_idempotent() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        host.apply(AudioCommand::AddLineSynth);
        // Set a param so we can detect whether the synth was replaced (a
        // replacement would reset bandpass_freq to its default).
        host.apply(AudioCommand::SetLineParam {
            key: "bandpass_freq",
            value: 4242.0,
        });
        // Second add should be a no-op; the param value survives.
        host.apply(AudioCommand::AddLineSynth);
        // We can't directly read the Shared from outside LineSynth, so the
        // best we can assert is `line_synth_active` remains true.
        assert!(host.line_synth_active());
    }

    #[test]
    fn remove_line_synth_is_idempotent() {
        let mut host = DspHost::new(48_000, 2, Vec::new());
        // Remove with nothing active should be a no-op.
        host.apply(AudioCommand::RemoveLineSynth);
        assert!(!host.line_synth_active());
        host.apply(AudioCommand::AddLineSynth);
        host.apply(AudioCommand::RemoveLineSynth);
        host.apply(AudioCommand::RemoveLineSynth);
        assert!(!host.line_synth_active());
    }

    // ----- Phase B: background sample mixing -----

    /// Build a deterministic stereo PCM buffer of N frames where L = +0.5
    /// and R = -0.5 on every frame. Lets us verify channel order and
    /// background mixing without depending on the OGG decoder.
    fn synthetic_stereo_pcm(frames: usize) -> Vec<f32> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            pcm.push(0.5);
            pcm.push(-0.5);
        }
        pcm
    }

    #[test]
    fn empty_background_falls_back_to_synth_only() {
        // With an empty buffer the render path takes the no-background
        // branch: synth-only output, identical to Phase A behavior.
        let mut host = DspHost::new(48_000, 2, Vec::new());
        assert!(!host.has_background());
        let mut buffer = vec![1.0_f32; 64];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn background_renders_when_synth_inactive() {
        // Even with no synth, the background should mix into the output
        // at the default volume of 1.0.
        let pcm = synthetic_stereo_pcm(64);
        let mut host = DspHost::new(48_000, 2, pcm);
        assert!(host.has_background());
        let mut buffer = vec![0.0_f32; 128]; // 64 stereo frames
        host.render(&mut buffer);
        // L channel = +0.5, R channel = -0.5 (master gain = 1.0).
        for frame in buffer.chunks(2) {
            assert!((frame[0] - 0.5).abs() < f32::EPSILON);
            assert!((frame[1] + 0.5).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn background_volume_scales_output() {
        let pcm = synthetic_stereo_pcm(32);
        let mut host = DspHost::new(48_000, 2, pcm);
        host.apply(AudioCommand::SetLineParam {
            key: "background_volume",
            value: 0.5,
        });
        assert!((host.background_volume() - 0.5).abs() < f32::EPSILON);
        let mut buffer = vec![0.0_f32; 64];
        host.render(&mut buffer);
        // L = 0.5 * 0.5 = 0.25, R = -0.5 * 0.5 = -0.25.
        for frame in buffer.chunks(2) {
            assert!((frame[0] - 0.25).abs() < f32::EPSILON);
            assert!((frame[1] + 0.25).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn background_volume_clamps_negative_to_zero() {
        let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(8));
        host.apply(AudioCommand::SetLineParam {
            key: "background_volume",
            value: -0.5,
        });
        assert!(host.background_volume() >= 0.0);
        // Negative volume would invert phase; the clamp guarantees zero.
        let mut buffer = vec![1.0_f32; 16];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn background_playhead_wraps_at_buffer_end() {
        // 4-frame buffer: first three frames carry +0.5/-0.5, last frame
        // carries +1.0/+1.0 (a marker we can detect on the wrap).
        let mut pcm = synthetic_stereo_pcm(3);
        pcm.push(1.0);
        pcm.push(1.0);
        let mut host = DspHost::new(48_000, 2, pcm);
        // Render 10 frames (= 2.5 loops). After 4 frames we should be back
        // at the start of the buffer.
        let mut buffer = vec![0.0_f32; 20];
        host.render(&mut buffer);
        // Frames 0..3 are the +0.5/-0.5 pattern.
        for frame in buffer[0..6].chunks(2) {
            assert!((frame[0] - 0.5).abs() < f32::EPSILON);
            assert!((frame[1] + 0.5).abs() < f32::EPSILON);
        }
        // Frame 3 is the +1.0/+1.0 marker.
        assert!((buffer[6] - 1.0).abs() < f32::EPSILON);
        assert!((buffer[7] - 1.0).abs() < f32::EPSILON);
        // Frame 4 wraps: back to the start (+0.5, -0.5).
        assert!((buffer[8] - 0.5).abs() < f32::EPSILON);
        assert!((buffer[9] + 0.5).abs() < f32::EPSILON);
        // Frame 7 is the marker again.
        assert!((buffer[14] - 1.0).abs() < f32::EPSILON);
        assert!((buffer[15] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn muted_zeros_background_too() {
        let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(16));
        host.apply(AudioCommand::SetMuted(true));
        let mut buffer = vec![0.0_f32; 32];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn background_clamps_when_synth_and_background_peak_together() {
        // Stereo PCM where every sample is +1.0 (background already at
        // the ceiling). Activate the synth and crank its volume so the
        // sum would exceed +1.0 without the clamp; assert that output
        // never exceeds the ceiling.
        let pcm = vec![1.0_f32; 32]; // 16 stereo frames, both channels +1.0.
        let mut host = DspHost::new(48_000, 2, pcm);
        host.apply(AudioCommand::AddLineSynth);
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        let mut buffer = vec![0.0_f32; 32];
        host.render(&mut buffer);
        for s in &buffer {
            assert!(s.abs() <= 1.0, "post-mix sample {s} escaped the clamp");
        }
    }

    #[test]
    fn background_volume_key_works_with_no_active_synth() {
        // Regression: background_volume must NOT route through LineSynth,
        // so it has to apply even when the synth is not active.
        let mut host = DspHost::new(48_000, 2, synthetic_stereo_pcm(4));
        assert!(!host.line_synth_active());
        host.apply(AudioCommand::SetLineParam {
            key: "background_volume",
            value: 0.0,
        });
        let mut buffer = vec![1.0_f32; 8];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }
}
