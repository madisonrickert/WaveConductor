//! Off-thread audio engine.
//!
//! ## Architecture
//!
//! ```text
//!   ┌─────────────────────────────┐        ┌────────────────────────────┐
//!   │ Bevy main thread (60 Hz)   │        │ cpal audio thread (kHz)    │
//!   │                             │        │                            │
//!   │  Sketch / nav system        │        │  cpal callback             │
//!   │   ↓ writes AudioCommand     │        │   ↑ pops AudioCommands     │
//!   │  NonSend<AudioCommandSender>──┼───────►│   ↓ ticks DspHost          │
//!   │                             │        │   ↓ writes samples to cpal │
//!   │  Res<AudioState>            │        │   ↑ pushes AudioMessage    │
//!   │   ↑ read by sketches/UI     │◄───────┼── pop_messages system      │
//!   │  NonSend<AudioMessageReceiver>│       │                            │
//!   └─────────────────────────────┘        └────────────────────────────┘
//! ```
//!
//! Both rings are lock-free (`rtrb`). The audio callback never allocates,
//! locks, or blocks; spec §5.4's real-time-friendly invariant.
//!
//! ## What systems consume
//!
//! - [`state::AudioState`] (`Res<…>`) — current engine status, sample rate,
//!   channel count, volume, mute state. Updated each `PreUpdate` from the
//!   audio→main message ring.
//! - [`ring::AudioCommandSender`] (`NonSendMut<…>`) — write
//!   [`command::AudioCommand`]s to mutate audio-thread state
//!   (`SetMasterVolume`, `SetMuted`). `NonSend` because `rtrb::Producer` is
//!   `Send` but not `Sync`; the resource is main-thread-only by construction.
//! - [`ring::AudioMessageReceiver`] (`NonSendMut<…>`) — raw access for systems
//!   that want low-level events; most systems can ignore this and just read
//!   `AudioState`.
//!
//! ## Lifecycle and home-screen silence
//!
//! The cpal stream is paused on [`AppState::Home`] via [`pause_audio_on_home`]
//! and resumed on exit via [`resume_audio_on_sketch`]. Bevy fires
//! `OnEnter(AppState::Home)` for the default state at app startup, so the
//! stream begins paused and no audio leaks onto the home screen.
//!
//! ## Default behavior
//!
//! With no sketches loaded, the audio engine runs silently — [`dsp::DspHost`]
//! defaults to a graph that emits zeros. Sketches in Plan 6+ will add their
//! own DSP graphs via `AudioCommand::AddSynth` (added when needed).

pub mod background;
pub mod command;
pub mod dsp;
pub mod engine;
pub mod line_synth;
pub mod nav;
pub mod ring;
pub mod state;

use bevy::ecs::system::NonSend;
use bevy::prelude::*;

use self::engine::AudioStream;
use self::state::AudioState;
use crate::lifecycle::state::AppState;

/// Single plugin that wires the audio engine into the Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`]. On `Startup`, builds the cpal stream,
/// spawns the DSP host, and installs the `Res<AudioCommandSender>` and
/// `Res<AudioMessageReceiver>` resources. On `PreUpdate`, drains the message
/// ring into `Res<AudioState>`. On `OnExit`, the `AudioStream` non-send
/// resource is dropped, which stops the cpal stream.
///
/// The cpal stream is paused while `AppState::Home` is active and resumed when
/// transitioning into any sketch state.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app
            // AudioState is always present so consumers can read it even before
            // the engine has started; status will be `NotStarted` until the
            // Startup system runs.
            .init_resource::<AudioState>()
            .add_systems(Startup, engine::start_audio_engine)
            .add_systems(PreUpdate, state::pump_audio_messages)
            .add_systems(Update, nav::handle_volume_toggle)
            // Pause the cpal device callback on Home; resume when entering any
            // sketch. OnEnter(Home) fires for the initial default state at
            // startup, so the stream begins paused.
            .add_systems(OnEnter(AppState::Home), pause_audio_on_home)
            .add_systems(OnExit(AppState::Home), resume_audio_on_sketch);
    }
}

/// `OnEnter(AppState::Home)` system — suspends the cpal stream.
///
/// Silences audio immediately on home-screen entry, including app startup
/// (Bevy fires `OnEnter` for the default state). The `Option<NonSend<…>>`
/// wrap handles the edge case where the audio engine failed to start.
pub fn pause_audio_on_home(stream: Option<NonSend<'_, AudioStream>>) {
    if let Some(stream) = stream {
        tracing::info!("AppState::Home entered — pausing cpal stream");
        stream.pause();
    }
}

/// `OnExit(AppState::Home)` system — resumes the cpal stream.
///
/// Called when transitioning from Home into any sketch state. The
/// `Option<NonSend<…>>` wrap handles the edge case where the audio engine
/// failed to start.
pub fn resume_audio_on_sketch(stream: Option<NonSend<'_, AudioStream>>) {
    if let Some(stream) = stream {
        tracing::info!("AppState::Home exited — resuming cpal stream");
        stream.play();
    }
}
