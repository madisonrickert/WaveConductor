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
//! - [`input::AudioAnalysis`] (`Res<…>`) — live audio-*input* analysis
//!   (RMS/bands/onset) for audio-reactive sketches; neutral unless a sketch
//!   has inserted [`input::AudioCaptureRequest`]. See the `input` module.
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
//!
//! ## Output-device failover is unconditional
//!
//! There is **no setting that enables or disables audio recovery** — a kiosk must
//! never run silent for hours because its endpoint blinked, so the whole failover
//! stack is always on: the cpal error callback's lock-free flag
//! ([`state::AudioErrorFlag`]), the device watcher's vanished-endpoint and
//! reappearance edges ([`device::drain_device_topology`]), the follow-the-default
//! migration ([`device::default_device_switched`]), the backoff supervisor
//! ([`supervisor::supervise_audio`], 1 s doubling to a 30 s cap, retrying
//! forever), and the post-rebuild synth-graph restore
//! ([`supervisor::SynthGraphReloadPending`]). The only related setting,
//! [`settings::AudioSettings::output_device`], picks *which* endpoint to prefer;
//! it never gates *whether* recovery runs.

pub mod background;
pub mod command;
pub mod cymatics_synth;
pub mod device;
pub mod dots_synth;
pub mod dsp;
pub mod engine;
pub mod flame_synth;
pub mod input;
pub mod line_synth;
pub mod nav;
pub mod ring;
pub mod sample_bank;
pub mod settings;
pub mod state;
pub mod supervisor;

use bevy::ecs::system::NonSend;
use bevy::prelude::*;

use self::engine::AudioStream;
use self::state::AudioState;
use crate::lifecycle::state::AppState;
use crate::settings::{RegisterRuntimeEnumOptionsExt, RegisterSketchSettingsExt};

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
/// other two. The watcher polls output-device topology every ~2 s — the name
/// list **and** the identity of the host's default endpoint — and sends a
/// snapshot, only when either actually changed, down an `mpsc` channel;
/// [`device::drain_device_topology`] (`PreUpdate`, main thread) moves the newest
/// one into `AvailableAudioDevices` / `DefaultOutputDevice` and asks
/// [`supervisor::AudioSupervisor`] for an immediate reconnect when the saved
/// endpoint reappears, or when the default switches while no device is pinned
/// (the event PA being plugged in). It is the thing that notices the TV came
/// back.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        // The persisted output-device choice. Registered here (not in a Startup
        // system) so the resource exists — loaded from disk — before
        // `start_audio_engine` runs and reads it: the first stream of the
        // session must open the operator's device, not the system default.
        app.register_sketch_settings::<settings::AudioSettings>();
        // Expose the watcher's live device list to Plan 03a's runtime-enum widget,
        // so `output_device` renders as a dropdown rather than a bare text field.
        // `AvailableAudioDevices` impls `RuntimeEnumOptionsSource` with
        // `OPTIONS_KEY = "audio_output_devices"`, matching the field's
        // `options_key`. The two string literals are pinned together by
        // `settings::tests::output_device_options_key_matches_its_options_source`,
        // and this registration by
        // `tests::the_output_device_fields_options_key_resolves_against_a_registered_source`
        // — a drift would render an empty dropdown, which is indistinguishable
        // from a TV that is merely asleep.
        app.register_runtime_enum_options::<device::AvailableAudioDevices>();

        // Audio *input* capture + analysis (Radiance Unit A). Registered here
        // so the input path is core audio plumbing, present in every app that
        // has audio output — sketches only insert/remove AudioCaptureRequest.
        app.add_plugins(input::AudioInputPlugin);
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
            .init_resource::<device::DefaultOutputDevice>()
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

        // Task 5R's bookkeeping: a rebuilt stream carries a voiceless `DspHost`
        // until the sketch re-enters its own state. Native-only, like the
        // supervisor that owns it.
        #[cfg(not(target_arch = "wasm32"))]
        app.init_resource::<supervisor::SynthGraphReloadPending>();

        // The reconnect supervisor (native only — it rebuilds a cpal stream).
        // The second sanctioned always-on system: a stream can die, or an
        // endpoint can finally appear, in *any* `AppState` — including `Idle`
        // and the attract screensaver, which is exactly the TV-wakes-up case —
        // so it cannot be gated on a sketch being active. Its quiet-frame cost
        // is three resource reads and a return; the blocking cpal calls happen
        // only on a backoff-gated rebuild attempt. See
        // `supervisor::supervise_audio`.
        //
        // `Update`, not `PreUpdate`, so the whole of this frame's `PreUpdate`
        // (the message pump, then the topology drain) has already landed: the
        // supervisor sees the freshest status *and* any `request_now` a
        // reappearing device asked for, in the same frame it arrived.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(Update, supervisor::supervise_audio);

        // Make an operator's device pick take effect *live* rather than at the
        // next launch, by routing it through the supervisor rather than opening a
        // device from the panel. Ordered before `supervise_audio` so a settled
        // change is rebuilt on the same frame it is made. It debounces (the
        // runtime-enum widget's free-text half writes per keystroke) and ignores a
        // name the host does not currently enumerate, so no partial name — and no
        // sleeping TV's name — ever reaches a cpal open. See
        // `settings::apply_output_device_change`.
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(
            Update,
            settings::apply_output_device_change.before(supervisor::supervise_audio),
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

/// Plugin-level wiring tests.
///
/// **These deliberately never call `app.update()`**: `AudioPlugin`'s `Startup`
/// system spawns the device-watcher OS thread and builds a cpal stream, neither of
/// which a headless CI runner has (or wants). `add_plugins` alone runs `build`,
/// which is where the wiring under test lives.
#[cfg(test)]
mod tests {
    use super::*;

    /// The half that `settings::output_device_options_key_matches_its_options_source`
    /// cannot see: that the options source is actually *registered* with the `App`,
    /// so the `output_device` field's declared `options_key` resolves to a real
    /// entry in 03a's snapshot. This is the condition
    /// `settings::runtime_enum::warn_on_unresolved_options_keys` warns about at
    /// startup in debug builds; asserting it here means CI fails on a broken wiring
    /// instead of a human having to spot a `warn!` line — and the runtime symptom
    /// (an empty dropdown, the saved name shown "(unavailable)") is exactly what a
    /// correctly-wired but *sleeping* TV looks like, so no one would spot it by eye.
    ///
    /// Note it checks the snapshot **entry**, not `options_for`'s slice: headless,
    /// with no watcher thread, the device list is legitimately empty — which is
    /// also precisely what an unresolved key returns. Only the key's presence
    /// distinguishes the two.
    #[test]
    fn the_output_device_fields_options_key_resolves_against_a_registered_source() {
        use crate::settings::runtime_enum::snapshot;
        use crate::settings::{SettingKind, SketchSettings};

        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(AudioPlugin);

        let Some(def) = settings::AudioSettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "output_device")
        else {
            unreachable!("the derive macro always emits a def for `output_device`");
        };
        let SettingKind::RuntimeEnum { options_key } = def.kind else {
            unreachable!("`output_device` is declared `ty = RuntimeEnum`");
        };

        let snap = snapshot(app.world());
        assert!(
            snap.iter().any(|entry| entry.options_key == options_key),
            "no registered RuntimeEnumOptionsSource reports `{options_key}`; \
             the audio-device dropdown would render empty"
        );
    }
}
