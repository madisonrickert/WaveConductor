//! cpal stream lifecycle and audio-thread wiring.
//!
//! The Startup system [`start_audio_engine`] builds:
//!   1. Two `rtrb` ring buffers (commands main → audio, messages audio → main).
//!   2. A [`super::dsp::DspHost`] sized to the device's default output config.
//!   3. A `cpal::Stream` whose data and error callbacks own the audio end of
//!      each ring plus the DSP host.
//!
//! The stream is wrapped in [`AudioStream`] (a non-send resource) so Bevy's
//! drop on app exit stops it cleanly. The producer end of the command ring and
//! the consumer end of the message ring become `Res<AudioCommandSender>` and
//! `Res<AudioMessageReceiver>` for any Bevy system to use.

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::command::{AudioCommand, AudioMessage};
use super::dsp::DspHost;
use super::ring::{AudioCommandSender, AudioMessageReceiver, RING_CAPACITY};
use super::state::AudioState;

/// Wraps the live `cpal::Stream` so Bevy keeps it alive for the app's
/// lifetime. `cpal::Stream` is `!Send` on macOS, hence the non-send resource.
pub struct AudioStream {
    /// Owned `cpal::Stream` handle. Never accessed after construction —
    /// dropping `AudioStream` stops the underlying audio thread. The leading
    /// underscore documents that intent to readers and silences the unused-
    /// field lint. Do not rename to remove the underscore.
    _stream: cpal::Stream,
}

/// Startup system. Builds the cpal stream and installs all engine resources.
///
/// On failure (no default output device, build error, play error) the system
/// logs the error and writes `AudioStatus::Errored` to `Res<AudioState>`. The
/// app continues to run silently; sketches that don't depend on audio remain
/// functional.
pub fn start_audio_engine(world: &mut World) {
    match build_engine() {
        Ok(built) => {
            // sender and receiver wrap rtrb::Producer/Consumer which are Send
            // but not Sync, so they are installed as non-send resources.
            world.insert_non_send_resource(built.sender);
            world.insert_non_send_resource(built.receiver);
            world.insert_non_send_resource(built.stream);
            world.resource_mut::<AudioState>().sample_rate = built.sample_rate;
            world.resource_mut::<AudioState>().channels = built.channels;
            // AudioState.status remains `NotStarted` until the audio thread
            // sends `StreamStarted` via the message ring, which the
            // pump_audio_messages system picks up on the next PreUpdate.
            tracing::info!(
                sample_rate = built.sample_rate,
                channels = built.channels,
                "audio engine started",
            );
        }
        Err(err) => {
            tracing::warn!(?err, "audio engine failed to start; running silently");
            world.resource_mut::<AudioState>().status = super::state::AudioStatus::Errored;
            world.resource_mut::<AudioState>().last_error = Some(err.to_string());
        }
    }
}

struct BuiltEngine {
    stream: AudioStream,
    sender: AudioCommandSender,
    receiver: AudioMessageReceiver,
    sample_rate: u32,
    channels: u16,
}

#[derive(Debug, thiserror::Error)]
enum EngineBuildError {
    #[error("no default output device available")]
    NoDefaultDevice,
    #[error("cpal default config error: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal stream build error: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal stream play error: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
}

fn build_engine() -> Result<BuiltEngine, EngineBuildError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(EngineBuildError::NoDefaultDevice)?;
    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.into();

    // Ring buffers. Producer for commands goes to main thread; consumer to
    // audio callback. Producer for messages goes to audio callback; consumer
    // to main thread.
    let (cmd_producer, mut cmd_consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
    let (mut msg_producer, msg_consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);

    let mut dsp = DspHost::new(sample_rate, channels);

    // Announce that the stream is up; the main thread's pump system will pick
    // this up and set AudioStatus::Running.
    let _ = msg_producer.push(AudioMessage::StreamStarted {
        sample_rate,
        channels,
    });

    // Send a clone of the producer into the error callback closure. Since cpal
    // gives us non-mutable access in the error closure, we need an alternate
    // path — but rtrb requires &mut for push. The pragmatic solution: log the
    // error via `tracing` from the callback; the main thread will not see a
    // structured `Errored` message in Plan 4. Plan 6+ can revisit if needed.
    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            // Drain commands.
            while let Ok(cmd) = cmd_consumer.pop() {
                dsp.apply(cmd);
                let echo = match cmd {
                    AudioCommand::SetMasterVolume(_) => AudioMessage::VolumeApplied(dsp.volume()),
                    AudioCommand::SetMuted(m) => AudioMessage::MutedApplied(m),
                };
                let _ = msg_producer.push(echo);
            }
            // Render.
            dsp.render(output);
        },
        move |err| {
            tracing::error!(?err, "cpal stream error");
        },
        None,
    )?;
    stream.play()?;

    Ok(BuiltEngine {
        stream: AudioStream { _stream: stream },
        sender: AudioCommandSender::new(cmd_producer),
        receiver: AudioMessageReceiver::new(msg_consumer),
        sample_rate,
        channels,
    })
}
