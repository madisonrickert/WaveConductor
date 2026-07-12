//! Integration tests for `AudioPlugin`.
//!
//! These tests exercise the main-thread side of the audio engine — message
//! pump, command sender, action handler — without bringing up a real
//! `cpal::Stream`. The `DspHost` itself is fully unit-tested in `audio/dsp.rs`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::match_wildcard_for_single_variants,
    reason = "expect, panic, and wildcard match are appropriate in test code"
)]

mod common;

use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::audio::{
    command::{AudioCommand, AudioMessage},
    device::{drain_device_topology, AvailableAudioDevices, BoundOutputDevice},
    ring::{AudioCommandSender, AudioMessageReceiver, RING_CAPACITY},
    state::{pump_audio_messages, AudioState, AudioStatus},
    supervisor::AudioSupervisor,
};

/// Construct a test app with the audio state, the message-pump system, and
/// the ring buffer resources, but without a real cpal stream.
fn test_app_with_audio_rings() -> (App, rtrb::Producer<AudioMessage>) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // Build rings manually so the test can drive the audio side directly.
    let (cmd_producer, _cmd_consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
    let (msg_producer, msg_consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);

    app.init_resource::<AudioState>();
    // sender/receiver are non-send resources (rtrb is Send but not Sync)
    app.insert_non_send(AudioCommandSender::new(cmd_producer));
    app.insert_non_send(AudioMessageReceiver::new(msg_consumer));
    app.add_systems(PreUpdate, pump_audio_messages);
    app.add_systems(Update, wc_core::audio::nav::handle_volume_toggle);

    (app, msg_producer)
}

#[test]
fn default_audio_state_is_not_started() {
    let (mut app, _msg_producer) = test_app_with_audio_rings();
    app.update();
    let state = app.world().resource::<AudioState>();
    assert_eq!(state.status, AudioStatus::NotStarted);
    assert!((state.volume - 1.0).abs() < f32::EPSILON);
    assert!(!state.muted);
}

#[test]
fn stream_started_message_updates_state() {
    let (mut app, mut msg_producer) = test_app_with_audio_rings();
    msg_producer
        .push(AudioMessage::StreamStarted {
            sample_rate: 48_000,
            channels: 2,
        })
        .expect("push");
    app.update();
    let state = app.world().resource::<AudioState>();
    assert_eq!(state.status, AudioStatus::Running);
    assert_eq!(state.sample_rate, 48_000);
    assert_eq!(state.channels, 2);
}

#[test]
fn errored_message_updates_state_and_status() {
    let (mut app, mut msg_producer) = test_app_with_audio_rings();
    msg_producer
        .push(AudioMessage::Errored("device unplugged".into()))
        .expect("push");
    app.update();
    let state = app.world().resource::<AudioState>();
    assert_eq!(state.status, AudioStatus::Errored);
    assert_eq!(state.last_error.as_deref(), Some("device unplugged"));
}

#[test]
fn volume_applied_message_mirrors_state() {
    let (mut app, mut msg_producer) = test_app_with_audio_rings();
    msg_producer
        .push(AudioMessage::VolumeApplied(0.25))
        .expect("push");
    app.update();
    let state = app.world().resource::<AudioState>();
    assert!((state.volume - 0.25).abs() < f32::EPSILON);
}

#[test]
fn muted_applied_message_mirrors_state() {
    let (mut app, mut msg_producer) = test_app_with_audio_rings();
    msg_producer
        .push(AudioMessage::MutedApplied(true))
        .expect("push");
    app.update();
    let state = app.world().resource::<AudioState>();
    assert!(state.muted);
}

/// `drain_device_topology` is registered unconditionally in `PreUpdate` — it has
/// to be, because a device can appear or vanish in any `AppState`. An always-on
/// system is only safe if **every** parameter it takes resolves in an app that
/// never started an audio engine (the headless harnesses), so this pins that: the
/// non-send `DeviceTopologyReceiver` is absent here, and the system must skip,
/// not fail param validation and bring the schedule down with it.
#[test]
fn drain_device_topology_is_inert_without_a_watcher() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // Exactly what `AudioPlugin::build` registers; nothing inserts the receiver.
    // `AudioState` is among them: the drain now *writes* it, because a bound
    // endpoint vanishing from a snapshot is a stream death cpal may never report.
    app.init_resource::<AudioState>();
    app.init_resource::<AvailableAudioDevices>();
    app.init_resource::<BoundOutputDevice>();
    app.init_resource::<AudioSupervisor>();
    app.add_systems(PreUpdate, drain_device_topology);

    app.update();
    app.update();

    assert!(
        app.world().resource::<AvailableAudioDevices>().0.is_empty(),
        "no watcher, no snapshots, no device list",
    );
    assert_eq!(
        app.world().resource::<AudioState>().status,
        AudioStatus::NotStarted,
        "and no snapshot means no vanished-device verdict either",
    );
}

/// Inject a physical key press via the shared `common::input` helpers, run one
/// update to process the press, then release and run another update so the
/// next press starts clean.
///
/// The test app must have a `Window` entity spawned before this is called
/// because `common::input::press_key` attaches the event to the first Window.
fn press_key(app: &mut App, key: KeyCode) {
    common::input::press_key(app, key);
    app.update();
    common::input::release_key(app, key);
    app.update();
}

#[test]
fn toggle_volume_action_pushes_set_muted_command() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // `common::input::press_key` attaches keyboard events to the first Window
    // entity. Spawn one so the helper does not panic.
    app.world_mut().spawn(Window::default());

    // Provide rings without a real stream; expose the command consumer locally
    // so the test can verify the action handler actually pushes.
    let (cmd_producer, mut cmd_consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
    let (_msg_producer, msg_consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);
    app.init_resource::<AudioState>();
    app.insert_non_send(AudioCommandSender::new(cmd_producer));
    app.insert_non_send(AudioMessageReceiver::new(msg_consumer));
    app.add_systems(Update, wc_core::audio::nav::handle_volume_toggle);

    app.update();
    assert!(
        cmd_consumer.pop().is_err(),
        "no command should be queued before the action fires",
    );

    // V key maps to ToggleVolume (per actions.rs default_input_map).
    press_key(&mut app, KeyCode::KeyV);

    let cmd = cmd_consumer
        .pop()
        .expect("ToggleVolume should push a command");
    match cmd {
        AudioCommand::SetMuted(m) => assert!(m, "first toggle should mute"),
        other => panic!("expected SetMuted, got {other:?}"),
    }

    // A second toggle, after the engine's message echo has updated AudioState
    // to muted=true, should flip back to unmuted. Simulate the echo manually
    // since the real audio thread isn't running.
    app.world_mut().resource_mut::<AudioState>().muted = true;
    press_key(&mut app, KeyCode::KeyV);
    let cmd = cmd_consumer.pop().expect("second toggle should push");
    match cmd {
        AudioCommand::SetMuted(m) => assert!(!m, "second toggle should unmute"),
        other => panic!("expected SetMuted, got {other:?}"),
    }
}

/// Task 5R's harness: a headless app carrying exactly the resources and systems
/// `AudioPlugin` registers for the supervisor, minus the cpal stream (CI has no
/// audio device, and none is needed — the reload is driven by the *pending* flag a
/// successful rebuild raises, which the test sets directly).
///
/// `AudioStatus::Running` with no `AudioStream` resource is the shape that keeps
/// `supervise_audio` off its reconnect path: it wants a cycle only on
/// `Reconnecting`, or on `NotStarted`/`Errored` with no stream. So the system runs
/// its Task 5R half and nothing else — no cpal call is ever reached.
#[cfg(not(target_arch = "wasm32"))]
fn test_app_with_supervisor() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // `LifecyclePlugin`'s action map reads `ButtonInput<KeyCode>`, which only
    // `InputPlugin` inserts.
    app.add_plugins(InputPlugin);
    app.add_plugins(StatesPlugin);
    // Brings `AppState`, `SketchReloadState`, and `drive_reload_state`.
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    app.init_resource::<AudioState>();
    app.init_resource::<AudioSupervisor>();
    app.init_resource::<wc_core::audio::supervisor::SynthGraphReloadPending>();
    app.world_mut().resource_mut::<AudioState>().status = AudioStatus::Running;
    app.add_systems(Update, wc_core::audio::supervisor::supervise_audio);
    app
}

/// Drive `AppState` to `state` and settle the transition.
#[cfg(not(target_arch = "wasm32"))]
fn enter_state(app: &mut App, state: wc_core::lifecycle::state::AppState) {
    app.world_mut()
        .resource_mut::<NextState<wc_core::lifecycle::state::AppState>>()
        .set(state);
    app.update();
    assert_eq!(
        *app.world()
            .resource::<State<wc_core::lifecycle::state::AppState>>()
            .get(),
        state,
    );
}

/// The defect Task 5R closes: a rebuilt stream carries a **fresh `DspHost` with
/// no synth voice**, because the only producers of `Add*Synth` are the sketches'
/// `OnEnter` systems and a stream rebuild does not re-run `OnEnter`. Mid-sketch,
/// the app therefore reported `Running`, played its transport, and made **no
/// sound** — on an unattended kiosk, indistinguishable from the outage it had just
/// recovered from.
///
/// So a successful rebuild (which raises `SynthGraphReloadPending`, simulated here)
/// must drive the reload round-trip, and the sketch's `OnEnter` must actually
/// re-fire — that re-run is the entire repair.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_successful_rebuild_mid_sketch_re_enters_the_sketch_to_restore_its_synth_graph() {
    use wc_core::audio::supervisor::SynthGraphReloadPending;
    use wc_core::lifecycle::reload::{ReloadPhase, ReloadReason, SketchReloadState};
    use wc_core::lifecycle::state::AppState;

    /// Stands in for the sketch's `OnEnter` synth-graph installer (the real one
    /// pushes `AddLineSynth`).
    #[derive(Resource, Default)]
    struct SynthAdds(u32);

    let mut app = test_app_with_supervisor();
    app.init_resource::<SynthAdds>();
    app.add_systems(
        OnEnter(AppState::Line),
        |mut adds: ResMut<'_, SynthAdds>| adds.0 += 1,
    );

    enter_state(&mut app, AppState::Line);
    assert_eq!(
        app.world().resource::<SynthAdds>().0,
        1,
        "precondition: the sketch installed its voice once on first entry",
    );

    // The stream dies and is rebuilt while the visitor is still in the sketch.
    // `rebuild_engine` returning true is what raises this flag.
    app.world_mut().resource_mut::<SynthGraphReloadPending>().0 = true;
    app.update();

    {
        let reload = app.world().resource::<SketchReloadState>();
        assert!(
            !reload.is_idle(),
            "the rebuild must have started a reload, not left the sketch voiceless",
        );
        assert_eq!(
            reload.reason,
            ReloadReason::AudioDeviceReconnect,
            "and it must carry the reconnect profile (instant, no audio dip)",
        );
        assert_eq!(reload.return_state, AppState::Line);
    }
    assert!(
        !app.world().resource::<SynthGraphReloadPending>().0,
        "the intent is spent, so it cannot fire a second round-trip",
    );

    // Let the round-trip run: FadeOut (zero-length) -> Home -> back into Line.
    for _ in 0..4 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
    );
    assert_eq!(
        app.world().resource::<SketchReloadState>().phase,
        ReloadPhase::Idle,
    );
    assert_eq!(
        app.world().resource::<SynthAdds>().0,
        2,
        "OnEnter must have re-fired exactly once — that re-run IS the synth-graph repair",
    );

    // And it fires *once* per rebuild: the reload changes `AppState`, not
    // `AudioState`, so nothing it does can arm another cycle. Many quiet frames
    // later, no second round-trip has started.
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<SynthAdds>().0,
        2,
        "no respawn loop: one successful rebuild buys exactly one re-entry",
    );
    assert!(app.world().resource::<SketchReloadState>().is_idle());
}

/// At `Home` there is no synth graph to restore, so the round-trip would be a
/// pointless `Home → Home` flicker. The intent is discarded, not spent — and not
/// left pending, which would fire it the instant a visitor picked a sketch.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_successful_rebuild_at_home_starts_no_reload() {
    use wc_core::audio::supervisor::SynthGraphReloadPending;
    use wc_core::lifecycle::reload::SketchReloadState;
    use wc_core::lifecycle::state::AppState;

    let mut app = test_app_with_supervisor();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Home
    );

    app.world_mut().resource_mut::<SynthGraphReloadPending>().0 = true;
    app.update();

    assert!(
        app.world().resource::<SketchReloadState>().is_idle(),
        "no sketch is running, so there is nothing to re-enter",
    );
    assert!(
        !app.world().resource::<SynthGraphReloadPending>().0,
        "and the intent is cleared, not left to fire on the next sketch entry",
    );
}

/// A **failed** rebuild owes nothing: there is no new `DspHost` to populate, so no
/// flag is raised and the sketch keeps the voice it already has. Pinned as the
/// negative of the test above — `supervise_audio` must not reload on every frame
/// it happens to run in a sketch.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn no_pending_flag_means_no_reload() {
    use wc_core::lifecycle::reload::SketchReloadState;
    use wc_core::lifecycle::state::AppState;

    let mut app = test_app_with_supervisor();
    enter_state(&mut app, AppState::Line);

    for _ in 0..10 {
        app.update();
    }
    assert!(
        app.world().resource::<SketchReloadState>().is_idle(),
        "nothing pending, nothing reloaded",
    );
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
}
