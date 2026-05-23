//! `DspHost` — the audio-thread DSP graph.
//!
//! Owns the master volume and mute state, processes incoming `AudioCommand`s,
//! and renders samples into the cpal output buffer. Fully unit-testable
//! without any audio hardware: construct, apply commands, render into a
//! `Vec<f32>`, assert.
//!
//! Plan 4 ships a default-silent host. Plan 6+ extends it with per-sketch DSP
//! graphs.

use super::command::AudioCommand;

/// DSP host owned by the cpal audio thread.
///
/// All state is plain `Copy`/`f32` so the audio callback never allocates.
#[derive(Debug, Clone)]
pub struct DspHost {
    /// Output sample rate in Hz. Stored for Plan 6+ synthesis graphs that need
    /// the rate for oscillator frequency calculations.
    #[allow(dead_code)]
    sample_rate: u32,
    /// Output channel count. Stored for Plan 6+ synthesis graphs that emit
    /// per-channel sample data.
    #[allow(dead_code)]
    channels: u16,
    volume: f32,
    muted: bool,
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
        }
    }

    /// Apply a command. Clamps and validates values that the type system
    /// can't constrain (e.g., `SetMasterVolume` outside `[0, 1]`).
    pub fn apply(&mut self, command: AudioCommand) {
        match command {
            AudioCommand::SetMasterVolume(v) => {
                self.volume = v.clamp(0.0, 1.0);
            }
            AudioCommand::SetMuted(m) => {
                self.muted = m;
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

    /// Render samples into `output`.
    ///
    /// `output` is a flat slice in cpal's interleaved layout: `[L, R, L, R, …]`
    /// for stereo. The buffer length is divisible by `channels`.
    ///
    /// Plan 4 default is silence (zeros). Plan 6+ replaces this with real
    /// synthesis from per-sketch DSP graphs.
    pub fn render(&mut self, output: &mut [f32]) {
        // Compute the effective gain once per buffer; both volume and mute
        // are buffer-level (no per-sample envelope yet).
        let gain = if self.muted { 0.0 } else { self.volume };
        // Future synthesis would write samples and then multiply by gain.
        // For now there are no sources, so the buffer is filled with zeros
        // (which is `0.0 * gain`).
        let _ = gain;
        for sample in output.iter_mut() {
            *sample = 0.0;
        }
    }
}

#[cfg(test)]
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
        let mut buffer = vec![0.5_f32; 64];
        host.render(&mut buffer);
        assert!(buffer.iter().all(|s| s.abs() < f32::EPSILON));
    }
}
