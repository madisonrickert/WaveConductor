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
//!   capture driver (Task 8) that builds/pauses/tears down the stream in
//!   response to `super::AudioCaptureRequest`.
//!
//! This file lands in two steps: the data-handoff types here (Task 6), then
//! the stream build + `drive_capture` driver (Task 8).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bevy::prelude::*;

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
