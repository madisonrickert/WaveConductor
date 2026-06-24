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

use super::background::{build_sample_bank, SampleAssets};
use super::command::{AudioCommand, AudioMessage};
use super::dsp::DspHost;
use super::ring::{AudioCommandSender, AudioMessageReceiver, RING_CAPACITY};
use super::state::AudioState;

/// Wraps the live `cpal::Stream` so Bevy keeps it alive for the app's
/// lifetime. `cpal::Stream` is `!Send` on macOS, hence the non-send resource.
///
/// Call [`pause`][Self::pause] / [`play`][Self::play] to suspend or resume the
/// cpal device callback without tearing down the stream. Both operations are
/// idempotent per cpal's contract.
pub struct AudioStream {
    /// Owned `cpal::Stream` handle. Dropping `AudioStream` stops the
    /// underlying audio thread.
    stream: cpal::Stream,
}

impl AudioStream {
    /// Suspend the cpal device callback.
    ///
    /// The DSP host and ring buffers are unaffected; audio resumes from where
    /// it left off when [`play`][Self::play] is called. Errors are logged with
    /// `tracing::warn!` rather than panicked — a failed pause leaves audio
    /// running, which is audible but not catastrophic.
    pub fn pause(&self) {
        if let Err(err) = self.stream.pause() {
            tracing::warn!(?err, "cpal stream pause failed");
        } else {
            tracing::debug!("cpal stream paused");
        }
    }

    /// Resume the cpal device callback after a [`pause`][Self::pause].
    ///
    /// Errors are logged with `tracing::warn!` rather than panicked — a failed
    /// play leaves the stream paused, which is silent but not catastrophic.
    pub fn play(&self) {
        if let Err(err) = self.stream.play() {
            tracing::warn!(?err, "cpal stream play failed");
        } else {
            tracing::debug!("cpal stream resumed");
        }
    }
}

/// Startup system. Builds the cpal stream, starts it, then immediately pauses
/// it. Installs all engine resources.
///
/// The stream starts paused so the home screen is always silent at launch,
/// regardless of `OnEnter(AppState::Home)` scheduling order. The
/// `OnExit(AppState::Home)` system calls `play()` when the first sketch loads.
///
/// On failure (no default output device, build error, play error) the system
/// logs the error and writes `AudioStatus::Errored` to `Res<AudioState>`. The
/// app continues to run silently; sketches that don't depend on audio remain
/// functional.
pub fn start_audio_engine(world: &mut World) {
    // Pull the encoded sample assets out of the world (if the binary crate
    // inserted them). We move them out so the encoded bytes are not retained
    // on the heap after the bank is built. `remove_resource` is
    // remove-if-present; `unwrap_or_default` gives an empty bank when the
    // resource was never inserted.
    let assets = world.remove_resource::<SampleAssets>().unwrap_or_default();

    match build_engine(&assets) {
        Ok(built) => {
            // sender and receiver wrap rtrb::Producer/Consumer which are Send
            // but not Sync, so they are installed as non-send resources.
            world.insert_non_send(built.sender);
            world.insert_non_send(built.receiver);
            world.insert_non_send(built.stream);
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

fn build_engine(assets: &SampleAssets) -> Result<BuiltEngine, EngineBuildError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(EngineBuildError::NoDefaultDevice)?;
    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.into();

    // Decode and resample all sample assets on the main thread before the
    // cpal callback starts. `build_sample_bank` logs and skips any entry
    // that fails to decode; the engine always starts even if assets are
    // missing or corrupt.
    let bank = build_sample_bank(assets, channels, sample_rate);
    tracing::info!(
        entries = assets.samples.len(),
        "sample bank built for audio engine"
    );

    // Ring buffers. Producer for commands goes to main thread; consumer to
    // audio callback. Producer for messages goes to audio callback; consumer
    // to main thread.
    let (cmd_producer, mut cmd_consumer) = rtrb::RingBuffer::<AudioCommand>::new(RING_CAPACITY);
    let (mut msg_producer, msg_consumer) = rtrb::RingBuffer::<AudioMessage>::new(RING_CAPACITY);

    let mut dsp = DspHost::new(sample_rate, channels, bank);

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
                // SetLineParam / SetDotsParam are fire-and-forget on the main
                // side; omit echoes to keep per-frame param sweeps off the
                // bounded message ring (which would otherwise drop them).
                let echo = match cmd {
                    AudioCommand::SetMasterVolume(_) => {
                        Some(AudioMessage::VolumeApplied(dsp.volume()))
                    }
                    AudioCommand::SetMuted(m) => Some(AudioMessage::MutedApplied(m)),
                    AudioCommand::AddLineSynth => Some(AudioMessage::LineSynthActivated),
                    AudioCommand::RemoveLineSynth => Some(AudioMessage::LineSynthDeactivated),
                    // Per-param sweeps are fire-and-forget; omit echoes to
                    // keep the bounded message ring from dropping them.
                    AudioCommand::SetLineParam { .. } | AudioCommand::SetDotsParam { .. } => None,
                    AudioCommand::AddDotsSynth => Some(AudioMessage::DotsSynthActivated),
                    AudioCommand::RemoveDotsSynth => Some(AudioMessage::DotsSynthDeactivated),
                };
                if let Some(msg) = echo {
                    let _ = msg_producer.push(msg);
                }
            }
            // Render.
            dsp.render(output);
        },
        move |err| {
            tracing::error!(?err, "cpal stream error");
        },
        None,
    )?;
    // Start the device callback so cpal registers the stream with the OS, then
    // immediately pause. The DSP host and ring buffers are ready; the
    // `OnExit(AppState::Home)` system calls `play()` when a sketch loads.
    // This guarantees silence on the home screen even if `OnEnter(AppState::Home)`
    // does not fire before the first rendered frame.
    stream.play()?;
    if let Err(err) = stream.pause() {
        tracing::warn!(
            ?err,
            "initial stream pause failed; audio may play on home screen"
        );
    } else {
        tracing::debug!("cpal stream started in paused state");
    }

    Ok(BuiltEngine {
        stream: AudioStream { stream },
        sender: AudioCommandSender::new(cmd_producer),
        receiver: AudioMessageReceiver::new(msg_consumer),
        sample_rate,
        channels,
    })
}
