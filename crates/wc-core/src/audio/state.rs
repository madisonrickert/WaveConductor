//! `Res<AudioState>` — main-thread snapshot of the audio engine.
//!
//! Updated each `PreUpdate` by [`pump_audio_messages`], which drains
//! `Res<AudioMessageReceiver>` into the fields below. Sketches and UI read this
//! resource; no other path is exposed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;
use bevy::reflect::Reflect;

use super::command::AudioMessage;
use super::ring::AudioMessageReceiver;

/// Human-facing `last_error` text set when the cpal error callback fires.
///
/// The callback itself cannot format the underlying `cpal::StreamError`
/// (formatting allocates, which is forbidden on that thread), so it only flips
/// a flag; the main thread substitutes this generic message.
const ERROR_CALLBACK_MESSAGE: &str =
    "cpal stream error callback fired (device disconnected or backend error)";

/// Lock-free flag shared with the cpal error callback.
///
/// The error callback runs on an OS audio thread and must not allocate, take a
/// lock, or log. When the stream dies mid-run it stores `true` here with a
/// single relaxed atomic write. [`pump_audio_messages`] observes (and clears)
/// the flag on the next `PreUpdate`, drives [`AudioStatus::Reconnecting`], and
/// logs the failure once on the main thread. Installed as a `Resource` by
/// [`super::engine::start_audio_engine`]; the same `Arc` is cloned into the
/// error-callback closure at stream-build time.
#[derive(Resource, Clone)]
pub struct AudioErrorFlag(pub Arc<AtomicBool>);

/// Lifecycle status of the audio engine, mirrored from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Default)]
pub enum AudioStatus {
    /// The Startup system has not yet run, or failed to build the stream.
    #[default]
    NotStarted,
    /// The audio thread is running and rendering samples.
    Running,
    /// The stream died mid-run (a device blip: TV asleep, input switch,
    /// endpoint removed) and the supervisor is rebuilding it on a backoff.
    /// This is a *recoverable* state; `AudioStatus::Errored` is not. See
    /// `supervisor::supervise_audio`.
    Reconnecting,
    /// The audio thread failed unrecoverably: no output device exists at all,
    /// or an explicit `AudioMessage::Errored`. See `last_error` in
    /// [`AudioState`].
    Errored,
}

/// Main-thread snapshot of audio-engine status.
///
/// `volume` and `muted` are the **target** state; the audio thread applies them
/// asynchronously after consuming the matching `AudioCommand`s, so a brief
/// out-of-sync window is possible. Treat the mismatch as harmless.
// Each sketch adds one `synth_active` bool. The lint fires at 4; suppressing
// it here is cleaner than encoding the activation bitmask in an integer or a
// richer state type for what is a simple mirror of audio-thread state.
#[allow(clippy::struct_excessive_bools)]
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
    /// Whether the Line synth is currently active on the audio thread.
    /// Mirrors `LineSynthActivated` / `LineSynthDeactivated` messages.
    pub line_synth_active: bool,
    /// Whether the Dots synth is currently active on the audio thread.
    /// Mirrors `DotsSynthActivated` / `DotsSynthDeactivated` messages.
    pub dots_synth_active: bool,
    /// Whether the Cymatics voice bundle is currently active on the audio thread.
    /// Mirrors `CymaticsSynthActivated` / `CymaticsSynthDeactivated` messages.
    pub cymatics_synth_active: bool,
    /// Whether the Flame synth is currently active on the audio thread.
    /// Mirrors `FlameSynthActivated` / `FlameSynthDeactivated` messages.
    pub flame_synth_active: bool,
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
            line_synth_active: false,
            dots_synth_active: false,
            cymatics_synth_active: false,
            flame_synth_active: false,
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
///
/// After draining the ring it checks [`AudioErrorFlag`]: if the cpal error
/// callback fired (the stream died mid-run), the flag is set. Observing it here
/// drives [`AudioStatus::Reconnecting`] and logs once. The error check runs
/// *after* the drain so a stream death takes precedence over any stale
/// `StreamStarted` that arrived in the same tick. The flag is optional so the
/// pump degrades cleanly when the engine failed to build (no flag resource
/// installed).
pub fn pump_audio_messages(
    mut state: ResMut<'_, AudioState>,
    mut receiver: NonSendMut<'_, AudioMessageReceiver>,
    error_flag: Option<Res<'_, AudioErrorFlag>>,
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
            AudioMessage::LineSynthActivated => {
                state.line_synth_active = true;
            }
            AudioMessage::LineSynthDeactivated => {
                state.line_synth_active = false;
            }
            AudioMessage::DotsSynthActivated => {
                state.dots_synth_active = true;
            }
            AudioMessage::DotsSynthDeactivated => {
                state.dots_synth_active = false;
            }
            AudioMessage::CymaticsSynthActivated => {
                state.cymatics_synth_active = true;
            }
            AudioMessage::CymaticsSynthDeactivated => {
                state.cymatics_synth_active = false;
            }
            AudioMessage::FlameSynthActivated => {
                state.flame_synth_active = true;
            }
            AudioMessage::FlameSynthDeactivated => {
                state.flame_synth_active = false;
            }
        }
    }

    // Surface a mid-run stream death. The error callback stores `true` and
    // never logs (real-time thread); `swap` consumes the flag so we act at most
    // once per error event, and `mark_reconnecting_from_callback` reports
    // whether this was the transition into `Reconnecting` so we log exactly
    // once. The supervisor (`supervisor::supervise_audio`) owns the rebuild
    // from here; this pump only flips the status so the supervisor picks it up.
    let callback_fired = error_flag
        .as_ref()
        .is_some_and(|flag| flag.0.swap(false, Ordering::Relaxed));
    if callback_fired && mark_reconnecting_from_callback(&mut state) {
        tracing::warn!(
            "cpal stream error callback fired; audio stream died. \
             Entering Reconnecting — the supervisor will rebuild it."
        );
    }
}

/// Drive [`AudioState`] into [`AudioStatus::Reconnecting`] in response to the
/// cpal error callback firing (a recoverable mid-run stream death).
///
/// Returns `true` only when this call *transitioned* the status into
/// `Reconnecting`, so the caller logs exactly once per failure rather than
/// every `PreUpdate` while the stream is down. Sets [`AudioState::last_error`]
/// to [`ERROR_CALLBACK_MESSAGE`] (the callback cannot format the underlying
/// error without allocating on its real-time thread).
///
/// A stream that is already `Reconnecting` (or has since gone `Errored` on a
/// hard failure) is left as-is and reports `false`.
pub(super) fn mark_reconnecting_from_callback(state: &mut AudioState) -> bool {
    let newly = state.status != AudioStatus::Reconnecting && state.status != AudioStatus::Errored;
    if newly {
        state.status = AudioStatus::Reconnecting;
    }
    state.last_error = Some(ERROR_CALLBACK_MESSAGE.to_string());
    newly
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

    #[test]
    fn callback_transitions_running_to_reconnecting_once() {
        let mut state = AudioState {
            status: AudioStatus::Running,
            ..AudioState::default()
        };
        // First observation transitions and reports `true` (so the caller logs).
        assert!(mark_reconnecting_from_callback(&mut state));
        assert_eq!(state.status, AudioStatus::Reconnecting);
        assert_eq!(state.last_error.as_deref(), Some(ERROR_CALLBACK_MESSAGE));
        // A second observation is idempotent and reports `false` (no re-log).
        assert!(!mark_reconnecting_from_callback(&mut state));
        assert_eq!(state.status, AudioStatus::Reconnecting);
    }

    #[test]
    fn error_flag_swap_consumes_the_flag() {
        let flag = AudioErrorFlag(Arc::new(AtomicBool::new(true)));
        // The pump consumes the flag with `swap`; the first read sees `true`,
        // subsequent reads see `false` until the callback sets it again.
        assert!(flag.0.swap(false, Ordering::Relaxed));
        assert!(!flag.0.swap(false, Ordering::Relaxed));
    }
}
