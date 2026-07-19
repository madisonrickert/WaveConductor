//! cpal input-stream lifecycle for the audio-input path.
//!
//! ## Thread model
//!
//! - The **cpal input thread** owns the producer end of a lock-free `rtrb`
//!   sample ring plus a clone of [`AudioInputErrorFlag`]'s atomic. Its data
//!   callback downmixes each frame to mono and pushes; its error callback
//!   stores one relaxed `true`. No allocation, no locks, no logging on
//!   either (the `audio::engine` discipline, in reverse).
//! - The **Bevy main thread** owns everything else: [`AudioInputRing`]
//!   (non-send consumer, drained by `analysis::drain_and_analyze`) and the
//!   capture driver ([`drive_capture`]) that builds/pauses/tears down the
//!   stream in response to `super::AudioCaptureRequest`.
//!
//! The data-handoff types ([`AudioInputRing`], [`AudioInputErrorFlag`],
//! [`AudioInputStatus`], [`CaptureRuntime`]) land above; the stream build,
//! `decide` policy table, and [`drive_capture`] driver land below. (`decide`
//! is crate-private, so it's a plain code span here rather than a link — the
//! `cargo doc` gate builds default features/visibility only and rejects a
//! public module doc linking to a lower-visibility item.)

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::analysis::{AnalysisEngine, AnalysisState};
use super::devices::default_host_fallible;
use super::AudioCaptureRequest;

/// Capacity of the input sample ring (mono f32 samples). ~340 ms at 48 kHz.
/// The `PreUpdate` drain empties it every frame (800 samples/frame at
/// 60 Hz), so this covers multi-frame render stalls; past that the callback
/// drops samples — analysis is best-effort, and a dropped sample is strictly
/// better than a blocked OS audio thread.
pub const RING_SAMPLE_CAPACITY: usize = 16_384;

/// Seconds between capture (re)build attempts after a failure. Keeps a
/// missing device from being probed every frame while still reacquiring a
/// kiosk mic within a couple of seconds of it reappearing.
pub const RETRY_COOLDOWN_S: f32 = 2.0;

/// Consumer end of the audio-input sample ring.
///
/// Installed as a **non-send** resource: `rtrb::Consumer` is `Send` but not
/// `Sync` (the same reasoning as `audio::ring`), so systems take it as
/// `NonSendMut`, pinning access to the main thread by construction.
pub struct AudioInputRing {
    /// Consumer half of the ring; the producer half lives in the cpal
    /// input callback.
    consumer: rtrb::Consumer<f32>,
}

impl AudioInputRing {
    /// Wrap the consumer half of an input ring. Called by the capture
    /// driver at stream build; also available to tests that construct rings
    /// manually without a real cpal stream.
    pub fn new(consumer: rtrb::Consumer<f32>) -> Self {
        Self { consumer }
    }

    /// Pop one sample, oldest first. `None` when the ring is empty.
    pub fn pop(&mut self) -> Option<f32> {
        self.consumer.pop().ok()
    }
}

/// Lock-free flag shared with the cpal input error callback (mirrors
/// `audio::state::AudioErrorFlag`). The callback runs on an OS audio thread
/// and must not allocate, lock, or log — it only stores `true` with a
/// relaxed atomic write. The capture driver swaps the flag each frame and
/// responds by tearing down and rebuilding the stream.
#[derive(Resource, Clone)]
pub struct AudioInputErrorFlag(pub Arc<AtomicBool>);

/// Diagnostic status of the audio-input capture path.
///
/// Written by the capture driver at event frequency only (build, teardown,
/// failure), so the `String`s in these variants are never per-frame
/// allocations. Read by diagnostics/dev UI; sketches should read
/// `super::AudioAnalysis` instead.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub enum AudioInputStatus {
    /// No `super::AudioCaptureRequest` present; capture torn down.
    #[default]
    Inactive,
    /// Capture is running.
    Running {
        /// Resolved cpal device name.
        device: String,
        /// Capture sample rate in Hz.
        sample_rate: u32,
    },
    /// Capture failed to build or died mid-run; retrying on a cooldown.
    Errored {
        /// Human-readable failure description.
        message: String,
    },
}

/// Main-thread bookkeeping for the capture driver.
///
/// Present from plugin build (`Default` = nothing running). All fields are
/// written at event frequency and read each frame by `drive_capture`
/// (Task 8).
#[derive(Resource, Default)]
pub struct CaptureRuntime {
    /// The *requested* device name the live stream was built for (`None` =
    /// system default). Compared against the current request each frame to
    /// detect device changes; the resolved cpal name lives in
    /// [`AudioInputStatus::Running`].
    pub current_device: Option<String>,
    /// Whether the live stream is currently paused.
    pub paused: bool,
    /// Whether the last build attempt failed (gates the retry cooldown).
    pub failed: bool,
    /// Seconds remaining before another build attempt is allowed.
    pub retry_timer: f32,
}

/// Wraps the live input `cpal::Stream` so Bevy keeps it alive. `cpal::Stream`
/// is `!Send` on macOS, hence a **non-send** resource — exactly like the
/// output engine's `audio::engine::AudioStream`.
pub struct AudioInputStream {
    /// Owned stream handle. Dropping it stops the OS input callback.
    stream: cpal::Stream,
}

impl AudioInputStream {
    /// Suspend the input callback. Errors are logged, never panicked — a
    /// failed pause leaves capture running, which is wasteful but harmless.
    pub fn pause(&self) {
        if let Err(err) = self.stream.pause() {
            tracing::warn!(?err, "cpal input stream pause failed");
        } else {
            tracing::debug!("cpal input stream paused");
        }
    }

    /// Resume the input callback after a pause. Errors are logged, never
    /// panicked — a failed play leaves analysis neutral, not broken.
    pub fn play(&self) {
        if let Err(err) = self.stream.play() {
            tracing::warn!(?err, "cpal input stream play failed");
        } else {
            tracing::debug!("cpal input stream resumed");
        }
    }
}

/// What `drive_capture` should do this frame. Derived by [`decide`] from
/// pure inputs so the policy is unit-testable without a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureAction {
    /// Nothing to do.
    None,
    /// Build a stream (none is running and one is wanted).
    Build,
    /// Tear the stream (and bookkeeping) down.
    Teardown,
    /// Tear down and immediately rebuild (device change or stream error).
    Rebuild,
    /// Pause the live stream.
    Pause,
    /// Resume the paused stream.
    Resume,
}

/// Pure inputs to [`decide`], gathered from the world each frame without
/// allocating (device names are compared as `&str`).
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is a distinct, independently-observed driver input \
              (stream liveness, pause state, error flag, failure state, retry \
              cooldown, the file-drive override) feeding one decision table, \
              not a state-machine flag set that would collapse into an enum"
)]
pub(crate) struct CaptureInputs<'a> {
    /// Whether `super::FileDriveActive` (`WC_AUDIO_FILE`) is in the world.
    /// Short-circuits [`decide`] to never build a real mic stream — see
    /// `super::file_drive`'s module docs ("Why the mic is suppressed, not
    /// paused") for the full seam. Fixed for the process's lifetime, so
    /// this is the first check, ahead of every other input.
    pub file_drive_active: bool,
    /// The current `AudioCaptureRequest`, if inserted.
    pub requested: Option<&'a AudioCaptureRequest>,
    /// Whether an `AudioInputStream` non-send resource is live.
    pub stream_alive: bool,
    /// The requested device name the live stream was built for.
    pub current_device: Option<&'a str>,
    /// Whether the live stream is paused.
    pub stream_paused: bool,
    /// Whether the cpal error callback fired since last frame.
    pub error_fired: bool,
    /// Whether the last build attempt failed.
    pub failed: bool,
    /// Whether the retry cooldown has elapsed (always true when not failed).
    pub retry_timer_elapsed: bool,
}

/// The capture driver's policy, as a pure function of [`CaptureInputs`].
pub(crate) fn decide(i: &CaptureInputs<'_>) -> CaptureAction {
    if i.file_drive_active {
        // WC_AUDIO_FILE is driving analysis instead of the mic (see
        // `super::file_drive`). A real capture stream is never built while
        // it's active, so `stream_alive`/`failed` can never be true here
        // either — the flag is read once at plugin build and fixed for the
        // process's lifetime, so there is nothing to ever tear down.
        return CaptureAction::None;
    }
    let Some(req) = i.requested else {
        // Nothing wanted: clear a live stream or a stale failure marker.
        return if i.stream_alive || i.failed {
            CaptureAction::Teardown
        } else {
            CaptureAction::None
        };
    };
    if !i.stream_alive {
        // Wanted but not running: build now, or wait out the failure cooldown.
        return if !i.failed || i.retry_timer_elapsed {
            CaptureAction::Build
        } else {
            CaptureAction::None
        };
    }
    if i.error_fired {
        // The stream died mid-run (device unplugged, backend error).
        return CaptureAction::Rebuild;
    }
    if req.device_name.as_deref() != i.current_device {
        // The operator picked a different device.
        return CaptureAction::Rebuild;
    }
    match (req.paused, i.stream_paused) {
        (true, false) => CaptureAction::Pause,
        (false, true) => CaptureAction::Resume,
        _ => CaptureAction::None,
    }
}

/// `PreUpdate` exclusive system: reconcile the live capture stream with
/// `super::AudioCaptureRequest` every frame.
///
/// Exclusive (`&mut World`) because building/tearing down inserts and
/// removes **non-send** resources, which `Commands` cannot do — the same
/// reason `audio::engine::start_audio_engine` is exclusive. The steady-state
/// cost with nothing to do is a handful of resource reads and one atomic
/// swap; all allocation (stream, rings, engine, name clones, status
/// strings) happens at event frequency inside the Build/Teardown arms.
///
/// Chained ahead of `analysis::drain_and_analyze` so a teardown or rebuild
/// is observed by the analysis system in the same frame.
pub fn drive_capture(world: &mut World) {
    // Tick the retry cooldown.
    let dt = world.resource::<Time>().delta_secs();
    {
        let mut runtime = world.resource_mut::<CaptureRuntime>();
        if runtime.retry_timer > 0.0 {
            runtime.retry_timer = (runtime.retry_timer - dt).max(0.0);
        }
    }
    // Consume the error flag (a swap, so one error yields one rebuild).
    let error_fired = world
        .get_resource::<AudioInputErrorFlag>()
        .is_some_and(|flag| flag.0.swap(false, std::sync::atomic::Ordering::Relaxed));

    let action = {
        let runtime = world.resource::<CaptureRuntime>();
        decide(&CaptureInputs {
            file_drive_active: world.get_resource::<super::FileDriveActive>().is_some(),
            requested: world.get_resource::<AudioCaptureRequest>(),
            stream_alive: world.get_non_send::<AudioInputStream>().is_some(),
            current_device: runtime.current_device.as_deref(),
            stream_paused: runtime.paused,
            error_fired,
            failed: runtime.failed,
            retry_timer_elapsed: runtime.retry_timer <= 0.0,
        })
    };

    match action {
        CaptureAction::None => {}
        CaptureAction::Pause => {
            if let Some(stream) = world.get_non_send::<AudioInputStream>() {
                stream.pause();
            }
            world.resource_mut::<CaptureRuntime>().paused = true;
        }
        CaptureAction::Resume => {
            if let Some(stream) = world.get_non_send::<AudioInputStream>() {
                stream.play();
            }
            world.resource_mut::<CaptureRuntime>().paused = false;
        }
        CaptureAction::Teardown => teardown_capture(world),
        CaptureAction::Build | CaptureAction::Rebuild => {
            teardown_capture(world);
            // Clone the requested name once, at build frequency. decide()
            // only returns Build/Rebuild when the request exists, and
            // nothing between there and here can remove it (exclusive
            // access), so this read is an invariant, not a race.
            let (device_name, start_paused) = {
                let Some(req) = world.get_resource::<AudioCaptureRequest>() else {
                    return;
                };
                (req.device_name.clone(), req.paused)
            };
            build_capture(world, device_name, start_paused);
        }
    }
}

/// Remove every capture-owned resource and reset the bookkeeping. Safe to
/// call when nothing is running (all removals are remove-if-present).
fn teardown_capture(world: &mut World) {
    let had_stream = world.remove_non_send::<AudioInputStream>().is_some();
    world.remove_non_send::<AudioInputRing>();
    world.remove_resource::<AudioInputErrorFlag>();
    world.remove_resource::<AnalysisState>();
    {
        let mut runtime = world.resource_mut::<CaptureRuntime>();
        runtime.current_device = None;
        runtime.paused = false;
        runtime.failed = false;
        runtime.retry_timer = 0.0;
    }
    *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Inactive;
    if had_stream {
        tracing::info!("audio input capture torn down");
    }
}

/// Build the capture stream and install every capture-owned resource; on
/// failure, record the error and arm the retry cooldown. All allocation in
/// here is at build frequency.
fn build_capture(world: &mut World, device_name: Option<String>, start_paused: bool) {
    match try_build_capture(device_name.as_deref()) {
        Ok(built) => {
            if start_paused {
                built.stream.pause();
            }
            tracing::info!(
                device = %built.resolved_name,
                sample_rate = built.sample_rate,
                channels = built.channels,
                "audio input capture started",
            );
            *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Running {
                device: built.resolved_name,
                sample_rate: built.sample_rate,
            };
            world.insert_resource(AudioInputErrorFlag(built.error_flag));
            world.insert_resource(AnalysisState(AnalysisEngine::new(built.sample_rate)));
            world.insert_non_send(built.ring);
            world.insert_non_send(built.stream);
            let mut runtime = world.resource_mut::<CaptureRuntime>();
            runtime.current_device = device_name;
            runtime.paused = start_paused;
            runtime.failed = false;
            runtime.retry_timer = 0.0;
        }
        Err(err) => {
            // Spec failure posture: neutral analysis (the drain system sees
            // no ring/engine), diagnostics via status, retry on a cooldown.
            // Never panic, never block, never fall back to a device the
            // operator did not pick.
            tracing::warn!(
                ?err,
                "audio input capture failed to start; analysis stays neutral"
            );
            *world.resource_mut::<AudioInputStatus>() = AudioInputStatus::Errored {
                message: err.to_string(),
            };
            let mut runtime = world.resource_mut::<CaptureRuntime>();
            runtime.current_device = None;
            runtime.failed = true;
            runtime.retry_timer = RETRY_COOLDOWN_S;
        }
    }
}

/// Everything a successful build hands back to the world-installing side.
struct BuiltCapture {
    stream: AudioInputStream,
    ring: AudioInputRing,
    error_flag: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    resolved_name: String,
}

/// Why a capture build failed. Event-frequency; formatting allocates, which
/// is fine off the audio thread.
#[derive(Debug, thiserror::Error)]
enum CaptureBuildError {
    #[error("no default input device available")]
    NoDefaultDevice,
    #[error("input device not found: {0}")]
    DeviceNotFound(String),
    #[error("cpal device enumeration error: {0}")]
    Devices(#[from] cpal::DevicesError),
    #[error("cpal default config error: {0}")]
    DefaultConfig(#[from] cpal::DefaultStreamConfigError),
    #[error("cpal stream build error: {0}")]
    BuildStream(#[from] cpal::BuildStreamError),
    #[error("cpal stream play error: {0}")]
    PlayStream(#[from] cpal::PlayStreamError),
    #[error("unsupported input sample format: {0}")]
    UnsupportedFormat(cpal::SampleFormat),
    #[error("audio host initialization failed: {0}")]
    HostUnavailable(#[from] cpal::HostUnavailable),
}

/// Resolve the device, size the ring, and build + start the cpal stream.
fn try_build_capture(device_name: Option<&str>) -> Result<BuiltCapture, CaptureBuildError> {
    // `default_host_fallible` (not `cpal::default_host()`, which panics
    // internally on host-init failure) — see its doc comment in
    // `devices.rs`; this module inherits the same "never panic" posture.
    let host = default_host_fallible()?;
    let device = match device_name {
        // None = system default input device (pinned contract).
        None => host
            .default_input_device()
            .ok_or(CaptureBuildError::NoDefaultDevice)?,
        // A named device must match exactly; absence is an error (retry
        // path), NOT a fallback to some other open mic.
        Some(name) => host
            .input_devices()?
            .find(|d| d.name().is_ok_and(|n| n == name))
            .ok_or_else(|| CaptureBuildError::DeviceNotFound(name.to_owned()))?,
    };
    let resolved_name = device
        .name()
        .unwrap_or_else(|_| String::from("<unnamed input device>"));
    let supported = device.default_input_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_SAMPLE_CAPACITY);
    let error_flag = Arc::new(AtomicBool::new(false));
    let stream = build_typed_stream(
        &device,
        &config,
        sample_format,
        producer,
        Arc::clone(&error_flag),
    )?;
    stream.play()?;

    Ok(BuiltCapture {
        stream: AudioInputStream { stream },
        ring: AudioInputRing::new(consumer),
        error_flag,
        sample_rate,
        channels,
        resolved_name,
    })
}

/// Build the stream for whichever sample format the device natively speaks,
/// converting to f32 in the callback. F32/I16/U16 cover every real backend
/// we target (`CoreAudio` is F32; WASAPI/ALSA commonly I16); anything exotic
/// errors cleanly into the retry path rather than guessing.
fn build_typed_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mut producer: rtrb::Producer<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<cpal::Stream, CaptureBuildError> {
    let channels = usize::from(config.channels);
    // Downmix scale, computed once here so the callback never divides.
    // f32::from(u16) is lossless; max(1) guards a zero-channel config.
    let inv_channels = 1.0 / f32::from(config.channels.max(1));
    // The error callback runs on an OS audio thread: no alloc, no lock, no
    // log — a single relaxed store, observed by drive_capture (the same
    // discipline as the output engine's error callback).
    match sample_format {
        cpal::SampleFormat::F32 => Ok(device.build_input_stream(
            config,
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                push_mono(data, channels, inv_channels, &mut producer, |s| s);
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        cpal::SampleFormat::I16 => Ok(device.build_input_stream(
            config,
            move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                // i16 -> f32 in [-1, 1): lossless From, scale by 1/32768.
                push_mono(data, channels, inv_channels, &mut producer, |s| {
                    f32::from(s) / 32_768.0
                });
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        cpal::SampleFormat::U16 => Ok(device.build_input_stream(
            config,
            move |data: &[u16], _info: &cpal::InputCallbackInfo| {
                // u16 -> f32 in [-1, 1): recenter around 32768 then scale.
                push_mono(data, channels, inv_channels, &mut producer, |s| {
                    (f32::from(s) - 32_768.0) / 32_768.0
                });
            },
            move |_err| {
                error_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            None,
        )?),
        other => Err(CaptureBuildError::UnsupportedFormat(other)),
    }
}

/// The input data callback's entire logic: downmix interleaved frames to
/// mono and push into the ring. Runs on the OS audio thread — no
/// allocation, no locks, no logging. A full ring drops the sample (the
/// `let _ =`): analysis is best-effort and the main thread drains every
/// frame, so sustained fullness only means rendering has stalled longer
/// than the ring covers (~340 ms at 48 kHz).
fn push_mono<T: Copy>(
    data: &[T],
    channels: usize,
    inv_channels: f32,
    producer: &mut rtrb::Producer<f32>,
    convert: impl Fn(T) -> f32,
) {
    if channels == 0 {
        return;
    }
    for frame in data.chunks_exact(channels) {
        let mut sum = 0.0_f32;
        for &s in frame {
            sum += convert(s);
        }
        let _ = producer.push(sum * inv_channels);
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::audio::input::AudioCaptureRequest;

    fn request(device: Option<&str>, paused: bool) -> AudioCaptureRequest {
        AudioCaptureRequest {
            device_name: device.map(String::from),
            paused,
        }
    }

    fn inputs(req: Option<&AudioCaptureRequest>) -> CaptureInputs<'_> {
        CaptureInputs {
            file_drive_active: false,
            requested: req,
            stream_alive: false,
            current_device: None,
            stream_paused: false,
            error_fired: false,
            failed: false,
            retry_timer_elapsed: true,
        }
    }

    // --- decide(): the driver's whole policy, as a pure table ---

    #[test]
    fn no_request_and_nothing_running_is_a_no_op() {
        assert_eq!(decide(&inputs(None)), CaptureAction::None);
    }

    #[test]
    fn no_request_with_a_live_stream_tears_down() {
        let mut i = inputs(None);
        i.stream_alive = true;
        assert_eq!(decide(&i), CaptureAction::Teardown);
    }

    #[test]
    fn no_request_with_a_failed_build_clears_the_failure() {
        let mut i = inputs(None);
        i.failed = true;
        assert_eq!(decide(&i), CaptureAction::Teardown);
    }

    #[test]
    fn a_request_with_no_stream_builds() {
        let req = request(None, false);
        assert_eq!(decide(&inputs(Some(&req))), CaptureAction::Build);
    }

    #[test]
    fn a_failed_build_waits_for_the_retry_cooldown() {
        let req = request(None, false);
        let mut i = inputs(Some(&req));
        i.failed = true;
        i.retry_timer_elapsed = false;
        assert_eq!(decide(&i), CaptureAction::None);
        i.retry_timer_elapsed = true;
        assert_eq!(decide(&i), CaptureAction::Build);
    }

    #[test]
    fn a_device_change_rebuilds() {
        let req = request(Some("USB Interface"), false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.current_device = Some("Built-in Microphone");
        assert_eq!(decide(&i), CaptureAction::Rebuild);
    }

    #[test]
    fn a_stream_error_rebuilds() {
        let req = request(None, false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.error_fired = true;
        assert_eq!(decide(&i), CaptureAction::Rebuild);
    }

    #[test]
    fn pause_state_follows_the_request() {
        let paused_req = request(None, true);
        let mut i = inputs(Some(&paused_req));
        i.stream_alive = true;
        assert_eq!(decide(&i), CaptureAction::Pause);

        let live_req = request(None, false);
        let mut i = inputs(Some(&live_req));
        i.stream_alive = true;
        i.stream_paused = true;
        assert_eq!(decide(&i), CaptureAction::Resume);
    }

    #[test]
    fn file_drive_active_suppresses_mic_capture_regardless_of_request() {
        // Even a fresh request that would normally Build, and a request
        // that would normally Rebuild on a device change, must both
        // collapse to None while WC_AUDIO_FILE is active — the whole point
        // of the seam (see `decide`'s file_drive_active short-circuit).
        let req = request(Some("USB Interface"), false);
        let mut i = inputs(Some(&req));
        i.file_drive_active = true;
        assert_eq!(decide(&i), CaptureAction::None);

        i.stream_alive = true;
        i.current_device = Some("Built-in Microphone");
        assert_eq!(
            decide(&i),
            CaptureAction::None,
            "a device change must not rebuild while file-drive is active"
        );

        i.error_fired = true;
        assert_eq!(
            decide(&i),
            CaptureAction::None,
            "a stream error must not rebuild while file-drive is active"
        );
    }

    #[test]
    fn a_healthy_matching_stream_is_a_no_op() {
        let req = request(Some("USB Interface"), false);
        let mut i = inputs(Some(&req));
        i.stream_alive = true;
        i.current_device = Some("USB Interface");
        assert_eq!(decide(&i), CaptureAction::None);
    }

    // --- push_mono(): the RT callback's only logic ---

    #[test]
    fn push_mono_downmixes_interleaved_stereo() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        // Two stereo frames: (1.0, 0.0) and (-0.5, -0.5).
        push_mono(&[1.0, 0.0, -0.5, -0.5], 2, 0.5, &mut producer, |s| s);
        assert!((consumer.pop().expect("frame 1") - 0.5).abs() < f32::EPSILON);
        assert!((consumer.pop().expect("frame 2") + 0.5).abs() < f32::EPSILON);
        assert!(consumer.pop().is_err(), "exactly two mono frames");
    }

    #[test]
    fn push_mono_converts_via_the_provided_closure() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(8);
        push_mono(&[i16::MAX, i16::MIN], 1, 1.0, &mut producer, |s| {
            f32::from(s) / 32_768.0
        });
        let a = consumer.pop().expect("first");
        let b = consumer.pop().expect("second");
        assert!(a > 0.999 && a <= 1.0);
        assert!((b + 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn push_mono_drops_samples_when_the_ring_is_full_without_panicking() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(4);
        let data = [0.1_f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        push_mono(&data, 1, 1.0, &mut producer, |s| s);
        // First 4 kept, overflow dropped silently.
        for expected in [0.1_f32, 0.2, 0.3, 0.4] {
            assert!((consumer.pop().expect("kept") - expected).abs() < f32::EPSILON);
        }
        assert!(consumer.pop().is_err());
    }

    #[test]
    fn push_mono_with_zero_channels_is_a_no_op() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(4);
        push_mono(&[0.5_f32], 0, 1.0, &mut producer, |s| s);
        assert!(consumer.pop().is_err());
        drop(producer);
    }

    // --- driver no-op path (headless-safe: no request is ever inserted) ---

    #[test]
    fn drive_capture_without_a_request_is_inert() {
        use bevy::prelude::*;
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<AudioInputStatus>();
        app.init_resource::<CaptureRuntime>();
        app.add_systems(PreUpdate, drive_capture);
        app.update();
        app.update();
        assert_eq!(
            *app.world().resource::<AudioInputStatus>(),
            AudioInputStatus::Inactive
        );
        assert!(app.world().get_non_send::<AudioInputStream>().is_none());
    }
}
