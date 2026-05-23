//! `Res<AudioState>` — main-thread snapshot of the audio engine.
//!
//! Updated each `PreUpdate` by [`pump_audio_messages`], which drains
//! `Res<AudioMessageReceiver>` into the fields below. Sketches and UI read this
//! resource; no other path is exposed.

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;
use bevy::reflect::Reflect;

use super::command::AudioMessage;
use super::ring::AudioMessageReceiver;

/// Lifecycle status of the audio engine, mirrored from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Default)]
pub enum AudioStatus {
    /// The Startup system has not yet run, or failed to build the stream.
    #[default]
    NotStarted,
    /// The audio thread is running and rendering samples.
    Running,
    /// The audio thread terminated with an error. See `last_error` in
    /// [`AudioState`].
    Errored,
}

/// Main-thread snapshot of audio-engine status.
///
/// `volume` and `muted` are the **target** state; the audio thread applies them
/// asynchronously after consuming the matching `AudioCommand`s, so a brief
/// out-of-sync window is possible. Treat the mismatch as harmless.
#[derive(Resource, Debug, Clone, Reflect)]
pub struct AudioState {
    /// Engine lifecycle status.
    pub status: AudioStatus,
    /// Sample rate the cpal stream is running at, in Hz. Zero before engine
    /// startup.
    pub sample_rate: u32,
    /// Output channel count (1 = mono, 2 = stereo, …).
    pub channels: u16,
    /// Master volume in `[0.0, 1.0]`. Multiplied into every output sample by
    /// the DSP host.
    pub volume: f32,
    /// Whether output is muted. When `true`, the DSP host overrides
    /// [`Self::volume`] with `0.0`.
    pub muted: bool,
    /// Most recent error from the audio thread, if any.
    pub last_error: Option<String>,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            status: AudioStatus::default(),
            sample_rate: 0,
            channels: 0,
            volume: 1.0,
            muted: false,
            last_error: None,
        }
    }
}

/// `PreUpdate` system that drains the audio→main ring into `Res<AudioState>`.
///
/// Reads every message that arrived since the last tick; the ring is bounded,
/// so under sustained load older messages may be dropped (the audio thread
/// uses `try_push` and accepts the loss — peak-level samples can afford it).
///
/// Uses `NonSendMut<AudioMessageReceiver>` because `rtrb::Consumer` is not
/// `Sync`; see `ring` module docs.
pub fn pump_audio_messages(
    mut state: ResMut<'_, AudioState>,
    mut receiver: NonSendMut<'_, AudioMessageReceiver>,
) {
    for msg in receiver.drain() {
        match msg {
            AudioMessage::StreamStarted {
                sample_rate,
                channels,
            } => {
                state.status = AudioStatus::Running;
                state.sample_rate = sample_rate;
                state.channels = channels;
                state.last_error = None;
            }
            AudioMessage::Errored(err) => {
                state.status = AudioStatus::Errored;
                state.last_error = Some(err);
            }
            AudioMessage::VolumeApplied(v) => {
                state.volume = v;
            }
            AudioMessage::MutedApplied(m) => {
                state.muted = m;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_not_started_unmuted_full_volume() {
        let state = AudioState::default();
        assert_eq!(state.status, AudioStatus::NotStarted);
        assert_eq!(state.sample_rate, 0);
        assert_eq!(state.channels, 0);
        assert!((state.volume - 1.0).abs() < f32::EPSILON);
        assert!(!state.muted);
        assert!(state.last_error.is_none());
    }
}
