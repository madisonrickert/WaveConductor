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

use super::command::AudioCommand;
use super::line_synth::LineSynth;

/// DSP host owned by the cpal audio thread.
///
/// All hot-path state is either plain `Copy`/`f32` or an `Option<Box<…>>`
/// whose presence is checked once per buffer.
#[derive(Debug)]
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
}

impl DspHost {
    /// Construct a default-silent host for the given output format.
    #[must_use]
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            volume: 1.0,
            muted: false,
            line_synth: None,
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
                if let Some(synth) = &self.line_synth {
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

    /// Render samples into `output`.
    ///
    /// `output` is a flat slice in cpal's interleaved layout: `[L, R, L, R, …]`
    /// for stereo. The buffer length is divisible by `channels`.
    ///
    /// Mixing:
    /// 1. The Line synth (if active) ticks one mono sample per output frame.
    /// 2. The sample is multiplied by `gain` (`muted ? 0 : volume`).
    /// 3. The result is splatted across all channels of the frame.
    ///
    /// When no voice graph is active, the buffer is filled with zeros.
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

        match self.line_synth.as_mut() {
            Some(synth) => {
                for frame in output.chunks_mut(channels) {
                    let sample = synth.tick_mono() * gain;
                    for slot in frame.iter_mut() {
                        *slot = sample;
                    }
                }
            }
            None => {
                for sample in output.iter_mut() {
                    *sample = 0.0;
                }
            }
        }
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
        let mut host = DspHost::new(48_000, 2);
        let mut buffer = vec![1.0_f32; 256];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }

    #[test]
    fn set_master_volume_clamps_range() {
        let mut host = DspHost::new(48_000, 2);
        host.apply(AudioCommand::SetMasterVolume(1.5));
        assert!((host.volume() - 1.0).abs() < f32::EPSILON);
        host.apply(AudioCommand::SetMasterVolume(-0.2));
        assert!(host.volume().abs() < f32::EPSILON);
        host.apply(AudioCommand::SetMasterVolume(0.5));
        assert!((host.volume() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn set_muted_updates_state() {
        let mut host = DspHost::new(48_000, 2);
        assert!(!host.muted());
        host.apply(AudioCommand::SetMuted(true));
        assert!(host.muted());
        host.apply(AudioCommand::SetMuted(false));
        assert!(!host.muted());
    }

    #[test]
    fn muted_render_outputs_zero_even_when_volume_high() {
        let mut host = DspHost::new(48_000, 2);
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
        let mut host = DspHost::new(48_000, 2);
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
        let mut host = DspHost::new(48_000, 2);
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
        let mut host = DspHost::new(48_000, 2);
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
        let mut host = DspHost::new(48_000, 2);
        // No synth active; SetLineParam should warn-and-drop, never panic.
        host.apply(AudioCommand::SetLineParam {
            key: "volume",
            value: 1.0,
        });
        assert!(!host.line_synth_active());
    }

    #[test]
    fn add_line_synth_is_idempotent() {
        let mut host = DspHost::new(48_000, 2);
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
        let mut host = DspHost::new(48_000, 2);
        // Remove with nothing active should be a no-op.
        host.apply(AudioCommand::RemoveLineSynth);
        assert!(!host.line_synth_active());
        host.apply(AudioCommand::AddLineSynth);
        host.apply(AudioCommand::RemoveLineSynth);
        host.apply(AudioCommand::RemoveLineSynth);
        assert!(!host.line_synth_active());
    }
}
