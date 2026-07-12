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
//! The cpal stream is **started in a paused state** by [`engine::start_audio_engine`]:
//! it calls `stream.play()` then immediately `stream.pause()` so the OS device
//! is registered but silent. This is the primary silence guarantee at launch.
//!
//! [`pause_audio_on_home`] (registered on `OnEnter(AppState::Home)`) provides
//! a secondary pause for runtime navigation back to Home after a sketch has run.
//! [`resume_audio_on_sketch`] (registered on `OnExit(AppState::Home)`) resumes
//! the stream when the user navigates into any sketch.
//!
//! ## Default behavior
//!
//! With no sketches loaded, the audio engine runs silently — [`dsp::DspHost`]
//! defaults to a graph that emits zeros. Sketches in Plan 6+ will add their
//! own DSP graphs via `AudioCommand::AddSynth` (added when needed).

pub mod background;
pub mod command;
pub mod cymatics_synth;
pub mod device;
pub mod dots_synth;
pub mod dsp;
pub mod engine;
pub mod flame_synth;
pub mod line_synth;
pub mod nav;
pub mod ring;
pub mod sample_bank;
pub mod state;
pub mod supervisor;

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
///
/// ## The third thread
///
/// Besides the main thread and cpal's audio thread, `Startup` also spawns the
/// **device-watcher** OS thread ([`device::spawn_device_watcher`]). cpal's device
/// enumeration can block (WASAPI especially), so it may sit on neither of the
/// other two. The watcher polls output-device topology every ~2 s and sends a
/// name snapshot — only when the list actually changed — down an `mpsc` channel;
/// [`device::drain_device_topology`] (`PreUpdate`, main thread) moves the newest
/// one into `AvailableAudioDevices` and asks [`supervisor::AudioSupervisor`] for
/// an immediate reconnect when the saved endpoint reappears. It is the thing that
/// notices the TV came back.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app
            // AudioState is always present so consumers can read it even before
            // the engine has started; status will be `NotStarted` until the
            // Startup system runs.
            .init_resource::<AudioState>()
            // Reconnect bookkeeping and the device picture the supervisor and
            // the settings panel read. Registered here (not by whichever system
            // happens to touch them first) so every system that takes them as a
            // `Res`/`ResMut` — including the always-on `drain_device_topology`
            // below — can rely on them existing from app build.
            .init_resource::<device::AvailableAudioDevices>()
            .init_resource::<device::BoundOutputDevice>()
            .init_resource::<supervisor::AudioSupervisor>()
            .add_systems(Startup, engine::start_audio_engine)
            .add_systems(PreUpdate, state::pump_audio_messages)
            // The egui keyboard-capture gate lives in the `emit_action_input`
            // producer (LifecyclePlugin / PreUpdate), so no `.run_if` is
            // needed here — the handler never sees a message while egui owns
            // the keyboard.
            .add_systems(Update, nav::handle_volume_toggle)
            // Pause the cpal device callback on Home; resume when entering any
            // sketch. The stream is already paused at engine start, so this
            // system's primary role is runtime Home re-entry (not startup).
            .add_systems(OnEnter(AppState::Home), pause_audio_on_home)
            .add_systems(OnExit(AppState::Home), resume_audio_on_sketch);

        // Device topology drain (native only — cpal enumeration is). This is one
        // of the sanctioned always-on systems: an endpoint can appear or vanish
        // in *any* `AppState`, including Idle and the attract screensaver (a TV
        // waking up is exactly that case), so it cannot be gated on a sketch
        // being active. It costs an empty-channel `try_recv` per frame and
        // returns; see `device::drain_device_topology`.
        //
        // Ordered after the message pump so a stream death observed this frame
        // has already moved `AudioState` into `Reconnecting` before a reappearing
        // device asks the supervisor to retry immediately.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            PreUpdate,
            device::drain_device_topology.after(state::pump_audio_messages),
        );
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
