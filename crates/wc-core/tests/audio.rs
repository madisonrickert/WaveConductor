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

use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::audio::{
    command::{AudioCommand, AudioMessage},
    ring::{AudioCommandSender, AudioMessageReceiver, RING_CAPACITY},
    state::{pump_audio_messages, AudioState, AudioStatus},
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
    app.insert_non_send_resource(AudioCommandSender::new(cmd_producer));
    app.insert_non_send_resource(AudioMessageReceiver::new(msg_consumer));
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

/// Inject a physical key press, run one update to process the press, then
/// release and run another update to process the release.
///
/// Uses `Buttonlike::press(world)` to inject a physical key event rather than
/// directly mutating `ActionState`, which leafwing's systems would overwrite.
/// Mirrors the pattern used in the lifecycle integration tests.
fn press_key(app: &mut App, key: KeyCode) {
    use leafwing_input_manager::user_input::Buttonlike;
    key.press(app.world_mut());
    app.update();
    key.release(app.world_mut());
    app.update(); // process the release so the next press starts clean
}

#[test]
fn toggle_volume_action_pushes_set_muted_command() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // Provide rings without a real stream; expose the command consumer locally
    // so the test can verify the action handler actually pushes.
    let (cmd_producer, mut cmd_consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
    let (_msg_producer, msg_consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);
    app.init_resource::<AudioState>();
    app.insert_non_send_resource(AudioCommandSender::new(cmd_producer));
    app.insert_non_send_resource(AudioMessageReceiver::new(msg_consumer));
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
