//! cpal stream lifecycle and audio-thread wiring.
//!
//! The Startup system [`start_audio_engine`] builds:
//!   1. Two `rtrb` ring buffers (commands main → audio, messages audio → main).
//!   2. A [`super::dsp::DspHost`] sized to the device's default output config.
//!   3. A `cpal::Stream` whose data callback owns the audio end of each ring
//!      plus the DSP host, and whose error callback owns a clone of a
//!      lock-free [`AudioErrorFlag`] it raises if the stream dies mid-run.
//!
//! The stream is wrapped in [`AudioStream`] (a non-send resource) so Bevy's
//! drop on app exit stops it cleanly. The producer end of the command ring and
//! the consumer end of the message ring become `Res<AudioCommandSender>` and
//! `Res<AudioMessageReceiver>` for any Bevy system to use.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::background::{build_sample_bank, SampleAssets};
use super::command::{AudioCommand, AudioMessage};
use super::dsp::DspHost;
use super::ring::{AudioCommandSender, AudioMessageReceiver, RING_CAPACITY};
use super::state::{AudioErrorFlag, AudioState};

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
/// ## A failed startup build is *not* terminal
///
/// A kiosk that powers on while its TV is still waking (or before anyone selects
/// the HDMI input) enumerates **no output device at all**. That is the routine
/// case, not an unrecoverable one: the endpoint appears seconds later. So a
/// failed build writes [`super::state::AudioStatus::Reconnecting`], not
/// `Errored`, and [`super::supervisor::supervise_audio`] picks the status up on
/// the next frame and starts a backoff cycle that re-attempts the (cheap)
/// enumeration until an endpoint exists. Latching `Errored` here — which is what
/// this system used to do — meant a silent installation for the night.
///
/// The app continues to run either way; sketches that don't depend on audio
/// remain functional.
pub fn start_audio_engine(world: &mut World) {
    // Read (do **not** remove) the encoded sample assets: a later stream rebuild
    // (`rebuild_engine`) has to re-decode them into a fresh `DspHost`, so the
    // encoded bytes must survive startup. Retaining the *compressed* bytes is a
    // small, bounded memory cost that buys mid-run reconnect. `get_resource` is
    // present-or-default; the binary crate inserts the real assets before
    // `Startup`, and headless tests never insert them at all.
    let assets = world
        .get_resource::<SampleAssets>()
        .cloned()
        .unwrap_or_default();

    // Spawn the device watcher *before* the build, and regardless of whether the
    // build succeeds. A boot with no output device at all — the kiosk powering on
    // while its TV is still asleep — is the routine case, not an error, and it is
    // exactly the case where nothing else will ever notice the endpoint arriving:
    // there is no stream, so there is no cpal error callback. The watcher is
    // independent of the stream and must outlive its failure to build.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (watcher, topology_rx) = super::device::spawn_device_watcher();
        // The watcher's `Drop` stops and joins the thread when the world is torn
        // down, so it cannot outlive the app.
        world.insert_resource(watcher);
        // `mpsc::Receiver` is Send but not Sync — main-thread-only, like the rings.
        world.insert_non_send(topology_rx);
    }

    // Recovery-only stage: no persisted device name yet (Task 6 wires it in), so
    // resolve to the system default. The enumeration is a one-shot main-thread
    // cost at `Startup`, never a per-frame one.
    let resolution = super::device::resolve_output_device(None, &host_output_names());

    match build_engine(&assets, &resolution) {
        Ok(built) => {
            let device_name = built.device_name.clone();
            // sender and receiver wrap rtrb::Producer/Consumer which are Send
            // but not Sync, so they are installed as non-send resources.
            world.insert_non_send(built.sender);
            world.insert_non_send(built.receiver);
            world.insert_non_send(built.stream);
            // Shared with the cpal error callback; `pump_audio_messages` reads
            // it each PreUpdate to surface a mid-run stream death.
            world.insert_resource(AudioErrorFlag(built.error_flag));
            world.resource_mut::<AudioState>().sample_rate = built.sample_rate;
            world.resource_mut::<AudioState>().channels = built.channels;
            // The endpoint this stream is actually bound to. The migrate-back
            // check (`saved_device_reappeared`) compares against it so it never
            // rebuilds a stream that is already on the saved device.
            world.resource_mut::<super::device::BoundOutputDevice>().0 = Some(device_name.clone());
            // Establish the settle baseline for the supervisor's flap defence:
            // the stream came up *now*, so a death within `STREAM_SETTLE_WINDOW`
            // counts as a flap rather than resetting the backoff. `Time<Real>` is
            // the monotonic clock the supervisor's contract requires.
            let now = world.resource::<Time<Real>>().elapsed_secs_f64();
            world
                .resource_mut::<super::supervisor::AudioSupervisor>()
                .record_success(now);
            // AudioState.status remains `NotStarted` until the audio thread
            // sends `StreamStarted` via the message ring, which the
            // pump_audio_messages system picks up on the next PreUpdate.
            tracing::info!(
                sample_rate = built.sample_rate,
                channels = built.channels,
                device = %device_name,
                "audio engine started",
            );
        }
        Err(err) => {
            // Recoverable, not terminal — see the doc comment. `Reconnecting` is
            // the state the supervisor drives; it will `begin()` a cycle on the
            // next frame and retry until an endpoint exists.
            tracing::warn!(
                ?err,
                "audio engine failed to start; entering Reconnecting — the supervisor will retry"
            );
            let mut state = world.resource_mut::<AudioState>();
            state.status = super::state::AudioStatus::Reconnecting;
            state.last_error = Some(err.to_string());
        }
    }
}

/// The host's output-device names, or an empty list when we could not ask.
///
/// **Can block** (enumeration); main-thread (re)build path only. An enumeration
/// *failure* and an empty device list are opposite facts to the topology differ
/// (see [`super::device::enumerate_output_names`]) — but not to the **resolver**,
/// which must pick something regardless and falls back to the host default while
/// keeping the operator's saved name. Collapsing `None` to `[]` is therefore
/// correct *here* and nowhere else.
fn host_output_names() -> Vec<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        super::device::enumerate_output_names(&cpal::default_host()).unwrap_or_default()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}

/// Rebuild the cpal stream after a mid-run death (or a first acquisition that
/// failed at startup), swap the engine resources, and restore transport and
/// mixer state. Returns `true` when a live stream now exists.
///
/// Exclusive main-thread access, called **only** from
/// [`super::supervisor::supervise_audio`] on a backoff-gated attempt — never per
/// frame. Re-resolves the device (Task 6 supplies the saved name; until then, the
/// host default), builds a fresh stream + rings + error flag, and replaces the
/// non-send `AudioStream`, `AudioCommandSender`, `AudioMessageReceiver`, and the
/// `AudioErrorFlag`. Inserting the new `AudioStream` drops the old one, which
/// stops the dead stream.
///
/// On failure it leaves the old (dead) resources in place, logs, and returns
/// `false`; the supervisor's already-armed backoff retries. This is why the
/// caller must not judge the outcome by "is an `AudioStream` present" — a dead
/// one still is.
///
/// ## What it restores, and what it does not
///
/// - **Transport**: `build_engine` hands back a paused stream (the home-silence
///   guarantee), so this resumes it only when a sketch is active. A rebuilt
///   stream left paused is silent, which looks exactly like the bug being fixed.
/// - **Mixer**: master volume and mute are re-pushed from [`AudioState`], because
///   the fresh `DspHost` starts at its defaults (volume 1.0, unmuted). Without
///   this, a reconnect would un-mute a muted kiosk at full volume.
/// - **Not the synth graph.** Each sketch issues its `Add*Synth` command from
///   `OnEnter(AppState::…)`, which a rebuild does not re-run, so the fresh
///   `DspHost` has no voices: mid-sketch, this returns `Running` and *silent*
///   until the visitor navigates away and back. The `*_synth_active` mirrors are
///   cleared here so `AudioState` tells the truth about that. Closing the gap is
///   Task 5R's job (re-enter the sketch state via the reload machine).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn rebuild_engine(world: &mut World) -> bool {
    use crate::lifecycle::state::AppState;

    let assets = world
        .get_resource::<SampleAssets>()
        .cloned()
        .unwrap_or_default();

    // Task 6 replaces this `None` with the persisted device name.
    let resolution = super::device::resolve_output_device(None, &host_output_names());

    let built = match build_engine(&assets, &resolution) {
        Ok(built) => built,
        Err(err) => {
            tracing::warn!(?err, "audio stream rebuild failed; will retry on backoff");
            return false;
        }
    };

    // Re-seed the mixer from the main-thread target state *before* the sender is
    // installed: the fresh DspHost is at its defaults, not the operator's.
    let (volume, muted) = {
        let state = world.resource::<AudioState>();
        (state.volume, state.muted)
    };
    let mut sender = built.sender;
    if sender
        .push(AudioCommand::SetMasterVolume(volume))
        .and_then(|()| sender.push(AudioCommand::SetMuted(muted)))
        .is_err()
    {
        // A freshly-created ring cannot be full; this is unreachable in practice.
        tracing::warn!("audio command ring full on rebuild; mixer state not restored");
    }

    let device_name = built.device_name.clone();
    world.insert_non_send(sender);
    world.insert_non_send(built.receiver);
    // Replaces (and therefore drops, and therefore stops) the dead stream.
    world.insert_non_send(built.stream);
    world.insert_resource(AudioErrorFlag(built.error_flag));
    world.resource_mut::<super::device::BoundOutputDevice>().0 = Some(device_name.clone());
    {
        let mut state = world.resource_mut::<AudioState>();
        state.sample_rate = built.sample_rate;
        state.channels = built.channels;
        state.status = super::state::AudioStatus::Running;
        state.last_error = None;
        // The fresh DspHost has no voices — see the doc comment. Tell the truth
        // until a sketch's `OnEnter` re-adds them.
        state.line_synth_active = false;
        state.dots_synth_active = false;
        state.cymatics_synth_active = false;
        state.flame_synth_active = false;
    }

    // Restore transport from AppState: paused at Home, playing in any sketch.
    let in_sketch = world.resource::<State<AppState>>().get().is_sketch();
    if in_sketch {
        if let Some(stream) = world.get_non_send::<AudioStream>() {
            stream.play();
        }
    }
    tracing::info!(device = %device_name, in_sketch, "audio stream rebuilt");
    true
}

struct BuiltEngine {
    stream: AudioStream,
    sender: AudioCommandSender,
    receiver: AudioMessageReceiver,
    /// Set by the cpal error callback when the stream dies mid-run; read by
    /// `pump_audio_messages`. Wrapped in [`AudioErrorFlag`] at install time.
    error_flag: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    /// Name of the output device this stream is bound to. Recorded into
    /// [`super::device::BoundOutputDevice`] so the migrate-back check knows the
    /// live endpoint.
    device_name: String,
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

/// Resolve a [`super::device::DeviceResolution`] to a concrete cpal device plus
/// its name.
///
/// `Preferred(name)` searches the host's output devices for an exact name match;
/// if the name has vanished since it was resolved (a race with a device blip) it
/// falls through to the host default rather than erroring — and, crucially,
/// **without rewriting the saved name**, which is not ours to touch (it lives in
/// the settings and the resolver only ever reads it as a `&str`). `Fallback`
/// opens the host default directly.
///
/// Errors with `NoDefaultDevice` when the host enumerates *no* output device at
/// all. For a kiosk that is a routine, recoverable state (the TV is still
/// waking), not a terminal one — see [`start_audio_engine`].
///
/// **Can block** (enumeration). Called only on the main thread (startup /
/// rebuild), never the audio callback and never the render thread.
fn open_output_device(
    host: &cpal::Host,
    resolution: &super::device::DeviceResolution,
) -> Result<(cpal::Device, String), EngineBuildError> {
    if let super::device::DeviceResolution::Preferred(name) = resolution {
        if let Ok(mut devices) = host.output_devices() {
            if let Some(device) = devices.find(|d| d.name().is_ok_and(|n| &n == name)) {
                return Ok((device, name.clone()));
            }
        }
        tracing::warn!(device = %name, "saved output device not found; using host default");
    }
    let device = host
        .default_output_device()
        .ok_or(EngineBuildError::NoDefaultDevice)?;
    // A device that cannot report its own name is still usable; only the
    // migrate-back bookkeeping cares about the string.
    let name = device.name().unwrap_or_else(|_| "default".to_owned());
    Ok((device, name))
}

fn build_engine(
    assets: &SampleAssets,
    resolution: &super::device::DeviceResolution,
) -> Result<BuiltEngine, EngineBuildError> {
    let host = cpal::default_host();
    let (device, device_name) = open_output_device(&host, resolution)?;
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

    // Lock-free signal for a mid-run stream death. cpal's error closure is
    // `FnMut` (no `&mut` access to the message ring, which `rtrb` needs to
    // push) and runs on an OS audio thread, so it must not allocate, lock, or
    // log. It only flips this flag with a single relaxed atomic store; the
    // main thread's `pump_audio_messages` observes it and drives
    // `AudioStatus::Reconnecting` — stream death is recoverable, not terminal.
    // One clone stays here (installed as `AudioErrorFlag`), the other moves
    // into the closure.
    let error_flag = Arc::new(AtomicBool::new(false));
    let error_flag_cb = Arc::clone(&error_flag);

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            // Drain commands.
            while let Ok(cmd) = cmd_consumer.pop() {
                dsp.apply(cmd);
                // SetLineParam / SetDotsParam / SetCymaticsParam /
                // TriggerCymaticsSample are fire-and-forget on the main side;
                // omit echoes to keep per-frame param sweeps off the bounded
                // message ring (which would otherwise drop them).
                let echo = match cmd {
                    AudioCommand::SetMasterVolume(_) => {
                        Some(AudioMessage::VolumeApplied(dsp.volume()))
                    }
                    AudioCommand::SetMuted(m) => Some(AudioMessage::MutedApplied(m)),
                    AudioCommand::AddLineSynth => Some(AudioMessage::LineSynthActivated),
                    AudioCommand::RemoveLineSynth => Some(AudioMessage::LineSynthDeactivated),
                    AudioCommand::AddDotsSynth => Some(AudioMessage::DotsSynthActivated),
                    AudioCommand::RemoveDotsSynth => Some(AudioMessage::DotsSynthDeactivated),
                    AudioCommand::AddCymaticsSynth => Some(AudioMessage::CymaticsSynthActivated),
                    AudioCommand::RemoveCymaticsSynth => {
                        Some(AudioMessage::CymaticsSynthDeactivated)
                    }
                    AudioCommand::AddFlameSynth => Some(AudioMessage::FlameSynthActivated),
                    AudioCommand::RemoveFlameSynth => Some(AudioMessage::FlameSynthDeactivated),
                    // Per-param sweeps and one-shot triggers are fire-and-forget;
                    // omit echoes to keep the bounded message ring from filling.
                    AudioCommand::SetLineParam { .. }
                    | AudioCommand::SetDotsParam { .. }
                    | AudioCommand::SetCymaticsParam { .. }
                    | AudioCommand::SetFlameParam { .. }
                    | AudioCommand::TriggerCymaticsSample(_) => None,
                };
                if let Some(msg) = echo {
                    let _ = msg_producer.push(msg);
                }
            }
            // Render.
            dsp.render(output);
        },
        move |_err| {
            // Real-time-sensitive thread: no alloc, no lock, no log. Formatting
            // `_err` would allocate and logging would take the tracing mutex, so
            // we only raise the flag. The main thread logs the failure once when
            // it observes the flag (see `pump_audio_messages`).
            error_flag_cb.store(true, Ordering::Relaxed);
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
        error_flag,
        sample_rate,
        channels,
        device_name,
    })
}
