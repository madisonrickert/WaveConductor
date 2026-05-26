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
//! ## Default behavior
//!
//! With no sketches loaded, the audio engine runs silently — [`dsp::DspHost`]
//! defaults to a graph that emits zeros. Sketches in Plan 6+ will add their
//! own DSP graphs via `AudioCommand::AddSynth` (added when needed).

pub mod command;
pub mod dsp;
pub mod engine;
pub mod line_synth;
pub mod nav;
pub mod ring;
pub mod state;

use bevy::prelude::*;

use self::state::AudioState;

/// Single plugin that wires the audio engine into the Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`]. On `Startup`, builds the cpal stream,
/// spawns the DSP host, and installs the `Res<AudioCommandSender>` and
/// `Res<AudioMessageReceiver>` resources. On `PreUpdate`, drains the message
/// ring into `Res<AudioState>`. On `OnExit`, the `AudioStream` non-send
/// resource is dropped, which stops the cpal stream.
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
            .add_systems(Update, nav::handle_volume_toggle);
    }
}
