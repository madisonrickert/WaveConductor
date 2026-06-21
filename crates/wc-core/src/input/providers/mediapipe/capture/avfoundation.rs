//! macOS webcam capture via `AVFoundation` on the maintained `objc2` framework
//! crates. Replaces nokhwa's `core-video-sys`/`objc 0.2` backend on macOS while
//! nokhwa keeps Linux/Windows. Frames arrive on a dispatch-queue delegate and
//! are drained by the worker through a single-slot [`LatestFrame`].
//!
//! Data flow: [`AvfFrameSource::open`] builds an `AVCaptureSession` (a camera
//! `AVCaptureDeviceInput` plus an `AVCaptureVideoDataOutput` requesting
//! `kCVPixelFormatType_32BGRA`) and installs a [`FrameDelegate`] on a serial
//! dispatch queue. Each captured `CMSampleBuffer` is locked, its BGRA bytes
//! copied into the shared [`LatestFrame`] slot, and unlocked — all on the
//! capture queue. The worker thread (which owns the `!Send` [`AvfFrameSource`])
//! drains that slot via [`FrameSource::next_frame`] / [`FrameSource::discard_frame`],
//! and lowers the *hardware* capture rate to [`IDLE_INFERENCE_HZ`] during the
//! idle throttle through [`FrameSource::set_capture_throttle`]. The only state
//! shared across the thread boundary is the `Arc<Mutex<LatestFrame>>`.
#![allow(dead_code)]
// backend wired into `open_camera_source` in Task 7.
// This file is the macOS AVFoundation FFI boundary: it is the one place in
// `wc-core` (besides the LeapC `unsafe impl`s) where the workspace
// `unsafe_code = "deny"` lint is lifted. Every `unsafe` block below carries an
// inline `// SAFETY:` note naming the objc2/CoreVideo/CoreMedia invariant it
// relies on.
#![allow(unsafe_code)]

use std::sync::{Arc, Mutex};

use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_av_foundation::{
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceInput,
    AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceTypeExternal, AVCaptureOutput, AVCaptureSession, AVCaptureSessionPreset640x480,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, kCVReturnSuccess, CVPixelBuffer,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use super::super::worker::IDLE_INFERENCE_HZ;
use super::{CaptureError, Frame, FrameSource};

/// Single-slot latest-frame handoff: the `AVFoundation` delegate `store`s the
/// newest BGRA frame; the worker drains it via `take_into`/`consume`. Behind an
/// `Arc<Mutex<_>>` shared between the dispatch queue and the worker thread.
#[derive(Default)]
pub(super) struct LatestFrame {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
    bytes_per_row: usize,
    /// Monotonic counter; a reader advances its own `last_gen` to this.
    generation: u64,
}

impl LatestFrame {
    /// Copy the newest BGRA frame in, reusing capacity (no realloc at steady
    /// size). Runs on the delegate's dispatch queue — a hot path; alloc-free.
    pub(super) fn store(&mut self, bgra: &[u8], width: u32, height: u32, bytes_per_row: usize) {
        self.bgra.clear();
        self.bgra.extend_from_slice(bgra);
        self.width = width;
        self.height = height;
        self.bytes_per_row = bytes_per_row;
        self.generation = self.generation.wrapping_add(1);
    }

    /// If a frame newer than `*last_gen` is present, repack it into `out`,
    /// advance `*last_gen`, and return `true`. Else return `false`.
    pub(super) fn take_into(&self, last_gen: &mut u64, out: &mut Frame) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        out.width = self.width;
        out.height = self.height;
        bgra_to_rgb(
            &self.bgra,
            self.bytes_per_row,
            self.width,
            self.height,
            &mut out.rgb,
        );
        *last_gen = self.generation;
        true
    }

    /// Like `take_into` but skips the repack — the worker's over-budget drain.
    pub(super) fn consume(&self, last_gen: &mut u64) -> bool {
        if self.generation == *last_gen {
            return false;
        }
        *last_gen = self.generation;
        true
    }
}

/// Choose which enumerated capture device to open. Returns `Some(index)` when
/// `requested` is in range, or `None` to fall back to the system default video
/// device — parity with nokhwa's `open(camera_index)` graceful fallback.
pub(super) fn select_device_index(device_count: usize, requested: u32) -> Option<usize> {
    let idx = usize::try_from(requested).ok()?;
    (idx < device_count).then_some(idx)
}

/// Human-readable label for the negotiated capture format (dev-panel diagnostics).
pub(super) fn format_label(width: u32, height: u32, fps: u32) -> String {
    format!("{width}x{height} BGRA @{fps}")
}

/// Repack camera BGRA (byte order B,G,R,A, possibly row-padded so
/// `bytes_per_row >= width*4`) into tightly-packed RGB8 in `out`.
///
/// `out` is resized to `width*height*3` and reused across frames (the worker
/// owns it). Only the first `width*4` bytes of each row are pixel data; the
/// remainder up to `bytes_per_row` is stride padding and is skipped.
pub(super) fn bgra_to_rgb(
    bgra: &[u8],
    bytes_per_row: usize,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) {
    let w = usize::try_from(width).unwrap_or(0);
    let h = usize::try_from(height).unwrap_or(0);
    out.clear();
    out.resize(w * h * 3, 0);
    for row in 0..h {
        let row_start = row * bytes_per_row;
        let src_row = &bgra[row_start..row_start + w * 4];
        let dst_row = &mut out[row * w * 3..(row + 1) * w * 3];
        for (px, rgb) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(3)) {
            rgb[0] = px[2]; // R
            rgb[1] = px[1]; // G
            rgb[2] = px[0]; // B
        }
    }
}

/// Instance variables for [`FrameDelegate`]: the single shared latest-frame
/// slot the delegate writes into. The `Arc<Mutex<_>>` is the only state that
/// crosses from the worker thread (which owns [`AvfFrameSource`]) to the
/// delegate's serial dispatch queue, so it carries all the synchronization.
struct FrameDelegateIvars {
    latest: Arc<Mutex<LatestFrame>>,
}

define_class!(
    // SAFETY:
    // - The superclass `NSObject` has no subclassing requirements.
    // - `FrameDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "WCAvfFrameDelegate"]
    #[ivars = FrameDelegateIvars]
    struct FrameDelegate;

    unsafe impl NSObjectProtocol for FrameDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
        // The capture queue calls this for every delivered video frame.
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn capture_output_did_output_sample_buffer(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            self.store_sample_buffer(sample_buffer);
        }
    }
);

impl FrameDelegate {
    /// Build a delegate that writes into `latest`.
    fn new(latest: Arc<Mutex<LatestFrame>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FrameDelegateIvars { latest });
        // SAFETY: standard `NSObject` designated-initializer chain on a freshly
        // allocated instance whose ivars were just initialized via `set_ivars`.
        unsafe { msg_send![super(this), init] }
    }

    /// Lock the sample buffer's BGRA pixel buffer, copy it into the shared
    /// [`LatestFrame`] slot, and unlock. Runs on the capture dispatch queue (a
    /// hot path): the only heap traffic is the slot's amortized `Vec` growth on
    /// the first/larger frame ([`LatestFrame::store`] reuses capacity after).
    fn store_sample_buffer(&self, sample_buffer: &CMSampleBuffer) {
        // SAFETY: `sample_buffer` is the live buffer AVFoundation handed to this
        // callback; `image_buffer()` borrows its `CVImageBuffer` (BGRA per our
        // `videoSettings`), or `None` if the buffer carries no pixel data.
        let Some(image_buffer) = (unsafe { sample_buffer.image_buffer() }) else {
            return;
        };
        // `CVImageBuffer` is a type alias of `CVPixelBuffer`; the deref coercion
        // from the retaining `CFRetained` wrapper yields the borrow we need.
        let pixel_buffer: &CVPixelBuffer = &image_buffer;

        // Guard against a non-BGRA buffer: `videoSettings` requests BGRA, but
        // never mis-read a surprise YUV plane as packed BGRA. (The CoreVideo
        // getters take `&CVPixelBuffer` and are safe wrappers, so no `unsafe`.)
        let pixel_format = CVPixelBufferGetPixelFormatType(pixel_buffer);
        if pixel_format != kCVPixelFormatType_32BGRA {
            return;
        }

        // SAFETY: lock the base address for read-only access before touching it;
        // CoreVideo guarantees the base address and stride stay stable until the
        // matching unlock below.
        let lock =
            unsafe { CVPixelBufferLockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly) };
        if lock != kCVReturnSuccess {
            // A lock failure leaves the slot holding the previous frame; the
            // worker simply sees no new generation this tick.
            return;
        }

        // SAFETY: between the successful lock and the unlock below, the base
        // address is a valid pointer to `bytes_per_row * height` bytes of BGRA
        // pixel data (row-major, possibly stride-padded). Width, height, and
        // stride are read from the same locked buffer, so the slice length is
        // correct and the bytes stay valid for the `store` copy.
        unsafe {
            let base = CVPixelBufferGetBaseAddress(pixel_buffer).cast::<u8>();
            let bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer);
            let width = CVPixelBufferGetWidth(pixel_buffer);
            let height = CVPixelBufferGetHeight(pixel_buffer);
            if !base.is_null() && bytes_per_row != 0 && width != 0 && height != 0 {
                // `len` and the slice use the native `usize` dims (no `as` cast);
                // only the `store` arguments narrow to `u32` via `try_from`.
                let len = bytes_per_row.saturating_mul(height);
                let bytes = std::slice::from_raw_parts(base, len);
                if let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) {
                    if let Ok(mut slot) = self.ivars().latest.lock() {
                        slot.store(bytes, w, h, bytes_per_row);
                    }
                }
            }
        }

        // SAFETY: balances the successful lock above with the same read-only
        // flags; required once per successful `CVPixelBufferLockBaseAddress`.
        unsafe { CVPixelBufferUnlockBaseAddress(pixel_buffer, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// macOS `AVFoundation` webcam backend. Holds the running `AVCaptureSession`
/// and the device handle on the worker thread; the delegate copies frames into
/// the shared [`LatestFrame`] slot from its dispatch queue.
///
/// `!Send` (it retains `!Send` `AVFoundation` objects), matching the
/// [`FrameSource`] contract that a source lives entirely on the worker thread.
/// Only `latest` crosses to the delegate queue.
pub struct AvfFrameSource {
    /// The running capture session (kept alive; stopped on drop).
    session: Retained<AVCaptureSession>,
    /// The capture device, locked/unlocked by [`Self::set_capture_throttle`].
    device: Retained<AVCaptureDevice>,
    /// The sample-buffer delegate; retained so it outlives the session.
    _delegate: Retained<FrameDelegate>,
    /// The serial callback queue; retained so it outlives the session.
    _queue: DispatchRetained<DispatchQueue>,
    /// Shared single-slot frame handoff, written by the delegate.
    latest: Arc<Mutex<LatestFrame>>,
    /// Last generation this source drained, advanced by `take_into`/`consume`.
    last_generation: u64,
    /// The device's active-format default min frame duration, cached so the
    /// idle throttle can restore the full capture rate when it lifts.
    full_rate_min_frame_duration: CMTime,
    /// Cached human-readable capture-format label for diagnostics.
    format: String,
}

impl AvfFrameSource {
    /// Open `camera_index` (falling back to the system default video device when
    /// the index is out of range), configure a 640x480 BGRA capture session, and
    /// start streaming frames to the delegate.
    ///
    /// # Errors
    /// Returns [`CaptureError::NoCamera`] when no camera matches / the device
    /// cannot be opened or added to the session.
    pub fn open(camera_index: u32) -> Result<Self, CaptureError> {
        // SAFETY: `AVMediaTypeVideo` is a framework-provided constant `NSString`,
        // valid for the process lifetime once AVFoundation is loaded.
        let media_video = unsafe { AVMediaTypeVideo }
            .ok_or_else(|| CaptureError::NoCamera("AVMediaTypeVideo unavailable".into()))?;

        // Enumerate built-in wide-angle + external video devices, then map the
        // requested index onto one (or fall back to the default video device).
        // SAFETY: both are framework-provided constant device-type `NSString`s.
        let device_types = NSArray::from_slice(&[
            unsafe { AVCaptureDeviceTypeBuiltInWideAngleCamera },
            unsafe { AVCaptureDeviceTypeExternal },
        ]);
        // SAFETY: discovery over a valid device-type array + video media type,
        // any position.
        let discovery = unsafe {
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &device_types,
                Some(media_video),
                AVCaptureDevicePosition::Unspecified,
            )
        };
        // SAFETY: returns a retained array of the discovered devices.
        let devices = unsafe { discovery.devices() };
        let device = match select_device_index(devices.len(), camera_index) {
            Some(idx) => devices.objectAtIndex(idx),
            // SAFETY: framework default video device, or `None` if no camera.
            None => unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media_video) }
                .ok_or_else(|| CaptureError::NoCamera("no video capture device".into()))?,
        };

        // SAFETY: fresh capture session.
        let session = unsafe { AVCaptureSession::new() };
        // SAFETY: `AVCaptureSessionPreset640x480` is a framework constant; setting
        // a supported preset on a not-yet-running session.
        unsafe { session.setSessionPreset(AVCaptureSessionPreset640x480) };

        // SAFETY: opens the device for capture; `Err(NSError)` if it cannot.
        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| CaptureError::NoCamera(format!("camera input: {e:?}")))?;
        // SAFETY: querying/adding an input on a not-yet-running session.
        if !unsafe { session.canAddInput(&input) } {
            return Err(CaptureError::NoCamera(
                "session rejects camera input".into(),
            ));
        }
        // SAFETY: `canAddInput` returned true immediately above.
        unsafe { session.addInput(&input) };

        // SAFETY: fresh video data output.
        let output = unsafe { AVCaptureVideoDataOutput::new() };
        // videoSettings = { kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }
        let pixel_format = NSNumber::numberWithUnsignedInt(kCVPixelFormatType_32BGRA);
        // SAFETY: `kCVPixelBufferPixelFormatTypeKey` is a framework constant
        // `CFString`, toll-free bridged to `NSString` via the `AsRef` impl.
        let key: &NSString = unsafe { kCVPixelBufferPixelFormatTypeKey }.as_ref();
        let value: &AnyObject = &pixel_format;
        let video_settings: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_slices(&[key], &[value]);
        // SAFETY: BGRA is a supported `videoSettings` pixel format.
        unsafe { output.setVideoSettings(Some(&video_settings)) };
        // SAFETY: drop late frames rather than queue them while the worker drains
        // newest-wins.
        unsafe { output.setAlwaysDiscardsLateVideoFrames(true) };

        let latest = Arc::new(Mutex::new(LatestFrame::default()));
        let delegate = FrameDelegate::new(Arc::clone(&latest));
        let queue = DispatchQueue::new("com.waveconductor.avf-capture", DispatchQueueAttr::SERIAL);
        let delegate_proto = ProtocolObject::from_ref(&*delegate);
        // SAFETY: `delegate` conforms to the sample-buffer delegate protocol; the
        // serial queue guarantees in-order, non-overlapping callbacks (required
        // for the single-slot handoff).
        unsafe { output.setSampleBufferDelegate_queue(Some(delegate_proto), Some(&queue)) };
        // SAFETY: querying/adding an output on a not-yet-running session.
        if !unsafe { session.canAddOutput(&output) } {
            return Err(CaptureError::NoCamera(
                "session rejects video output".into(),
            ));
        }
        // SAFETY: `canAddOutput` returned true immediately above.
        unsafe { session.addOutput(&output) };

        // Cache the active-format defaults for the format label and throttle.
        // SAFETY: the opened device's current active format (retained).
        let active_format = unsafe { device.activeFormat() };
        // SAFETY: the active format's `CMFormatDescription` (a video format
        // description, whose dimensions we read below).
        let format_desc = unsafe { active_format.formatDescription() };
        // SAFETY: `format_desc` is a valid video format description.
        let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&format_desc) };
        let width = u32::try_from(dims.width).unwrap_or(0);
        let height = u32::try_from(dims.height).unwrap_or(0);
        // SAFETY: the active format's supported frame-rate ranges (retained).
        let ranges = unsafe { active_format.videoSupportedFrameRateRanges() };
        let fps = match ranges.len() {
            0 => 0,
            // SAFETY: index 0 is in range; `maxFrameRate` reads the range's cap.
            _ => round_fps(unsafe { ranges.objectAtIndex(0).maxFrameRate() }),
        };
        // SAFETY: the device's current active min frame duration; cached so the
        // throttle can restore the full capture rate when idle lifts.
        let full_rate_min_frame_duration = unsafe { device.activeVideoMinFrameDuration() };
        let format = format_label(width, height, fps);

        // SAFETY: begin capture; frames now flow to the delegate queue.
        unsafe { session.startRunning() };

        Ok(Self {
            session,
            device,
            _delegate: delegate,
            _queue: queue,
            latest,
            last_generation: 0,
            full_rate_min_frame_duration,
            format,
        })
    }
}

impl FrameSource for AvfFrameSource {
    fn format_label(&self) -> Option<&str> {
        Some(&self.format)
    }

    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        let slot = self
            .latest
            .lock()
            .map_err(|_| CaptureError::Read("frame slot poisoned".into()))?;
        Ok(slot.take_into(&mut self.last_generation, out))
    }

    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        let slot = self
            .latest
            .lock()
            .map_err(|_| CaptureError::Read("frame slot poisoned".into()))?;
        Ok(slot.consume(&mut self.last_generation))
    }

    fn set_capture_throttle(&mut self, throttled: bool) {
        // Cap the *hardware* capture rate to `IDLE_INFERENCE_HZ` while idle (so
        // the sensor/ISP shed work), restoring the cached full-rate duration when
        // the throttle lifts.
        let target = if throttled {
            // The unclamped `1 / IDLE_INFERENCE_HZ`s idle target may fall outside
            // the active format's supported frame-duration range (e.g. a fixed
            // 30 fps webcam whose only range is 30..=30 fps). Setting an
            // out-of-range `activeVideoMinFrameDuration` raises an uncatchable
            // Objective-C exception (process abort) that the `lockForConfiguration`
            // `Result` guard below does NOT catch, so clamp into range first.
            // `None` => no usable range; skip the throttle and leave the camera at
            // full rate rather than risk the abort.
            if let Some(clamped) = self.clamped_idle_min_frame_duration() {
                clamped
            } else {
                tracing::warn!(
                    "avf: no usable supported frame-rate range; skipping capture throttle"
                );
                return;
            }
        } else {
            self.full_rate_min_frame_duration
        };
        // SAFETY: take exclusive configuration access before mutating a hardware
        // property; `Err` if another client holds it.
        if unsafe { self.device.lockForConfiguration() }.is_err() {
            // Non-fatal: skip this throttle change rather than panic on the worker
            // thread; the worker's decode-skipping still sheds most of the load.
            tracing::warn!("avf: lockForConfiguration failed; skipping capture-throttle change");
            return;
        }
        // SAFETY: the device is locked for configuration. `target` is either the
        // cached active-format default (restore path) or the idle target already
        // clamped into the active format's supported frame-duration range by
        // `clamped_idle_min_frame_duration` above, so the value is guaranteed in
        // range and cannot trigger the out-of-range abort.
        unsafe { self.device.setActiveVideoMinFrameDuration(target) };
        // SAFETY: balances the successful lock above.
        unsafe { self.device.unlockForConfiguration() };
    }
}

impl AvfFrameSource {
    /// Compute the idle min-frame-duration `CMTime` clamped into the device's
    /// active-format supported range, or `None` when no usable range is available
    /// (empty array or non-finite durations) — in which case the caller leaves the
    /// camera at full rate rather than risk an out-of-range set.
    ///
    /// Reads the active format's `videoSupportedFrameRateRanges`, takes the union
    /// `[min minFrameDuration, max maxFrameDuration]` across its ranges (in
    /// seconds), clamps the `1 / IDLE_INFERENCE_HZ`s idle target into it via the
    /// pure [`clamp_idle_frame_duration_secs`], and rebuilds the result as a
    /// `CMTime` at the [`IDLE_INFERENCE_HZ`] timescale.
    fn clamped_idle_min_frame_duration(&self) -> Option<CMTime> {
        // SAFETY: the device's current active format (retained); valid for the
        // life of the returned `Retained` handle.
        let active_format = unsafe { self.device.activeFormat() };
        // SAFETY: the active format's supported frame-rate ranges (retained array
        // of immutable `AVFrameRateRange`s).
        let ranges = unsafe { active_format.videoSupportedFrameRateRanges() };
        if ranges.is_empty() {
            return None;
        }

        // Union of all supported ranges, in seconds: the shortest minFrameDuration
        // (fastest rate) to the longest maxFrameDuration (slowest rate). Any
        // non-finite CMTime (invalid/indefinite → NaN from `seconds`) poisons the
        // bound and disqualifies the throttle.
        let mut min_supported = f64::INFINITY;
        let mut max_supported = f64::NEG_INFINITY;
        for range in &ranges {
            // SAFETY: `range` is a live `AVFrameRateRange`; `minFrameDuration` /
            // `maxFrameDuration` are its immutable `CMTime` properties, and
            // `CMTime::seconds` is a pure value conversion over a `Copy` struct
            // (NaN for an invalid/indefinite time, handled by `is_finite` below).
            let lo = unsafe { range.minFrameDuration().seconds() };
            let hi = unsafe { range.maxFrameDuration().seconds() };
            if !lo.is_finite() || !hi.is_finite() {
                return None;
            }
            min_supported = min_supported.min(lo);
            max_supported = max_supported.max(hi);
        }
        if !min_supported.is_finite() || !max_supported.is_finite() {
            return None;
        }

        let desired = 1.0 / f64::from(IDLE_INFERENCE_HZ);
        let clamped = clamp_idle_frame_duration_secs(desired, min_supported, max_supported);
        if !clamped.is_finite() || clamped <= 0.0 {
            return None;
        }

        let timescale = i32::try_from(IDLE_INFERENCE_HZ).unwrap_or(i32::MAX);
        // SAFETY: `CMTime::with_seconds` (`CMTimeMakeWithSeconds`) is a pure value
        // construction from a finite, positive seconds value and a positive
        // timescale; it touches no pointers or Objective-C objects. The result is
        // the clamped idle duration, guaranteed within the format's supported
        // range by `clamp_idle_frame_duration_secs` above.
        Some(unsafe { CMTime::with_seconds(clamped, timescale) })
    }
}

impl Drop for AvfFrameSource {
    fn drop(&mut self) {
        // SAFETY: stop the running session on teardown to release the camera and
        // halt delegate callbacks before the shared slot is dropped.
        unsafe { self.session.stopRunning() };
    }
}

/// Round a `CoreMedia` frame rate (frames/second as `f64`) to the nearest whole
/// `u32` for the diagnostic format label. Non-finite or out-of-range rates clamp
/// into `0..=u32::MAX`.
fn round_fps(rate: f64) -> u32 {
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rate is clamped to [0, u32::MAX] then rounded; f64 -> u32 has \
                  no From/TryFrom"
    )]
    {
        rate.clamp(0.0, f64::from(u32::MAX)).round() as u32
    }
}

/// Clamp the idle min-frame-duration target (in seconds) into the active
/// format's supported `[min_supported, max_supported]` duration range.
///
/// `AVFoundation` requires `activeVideoMinFrameDuration` to lie within the
/// active format's `videoSupportedFrameRateRanges`; setting an out-of-range
/// value raises an uncatchable Objective-C exception (process abort). This pure
/// helper establishes the in-range invariant the setter relies on:
/// - a `desired` longer than `max_supported` (the abort case — e.g. a 1/4s idle
///   target on a fixed 30 fps camera whose max duration is 1/30s) clamps down to
///   `max_supported`, making the idle throttle a no-op there rather than aborting;
/// - a `desired` shorter than `min_supported` clamps up to `min_supported`;
/// - a `desired` already inside the range passes through unchanged.
///
/// Note the duration/rate inversion: a *longer* min frame duration means a
/// *lower* (slower) capture rate. The idle target wants a long duration; the
/// clamp keeps it no longer than the slowest rate the format supports.
pub(super) fn clamp_idle_frame_duration_secs(
    desired: f64,
    min_supported: f64,
    max_supported: f64,
) -> f64 {
    desired.clamp(min_supported, max_supported)
}

/// The idle capture cap as a `CMTime` min frame duration: `1 / IDLE_INFERENCE_HZ`
/// seconds, so the hardware rate matches the worker's idle inference rate
/// exactly (value `1` over timescale [`IDLE_INFERENCE_HZ`]).
fn idle_min_frame_duration() -> CMTime {
    let timescale = i32::try_from(IDLE_INFERENCE_HZ).unwrap_or(i32::MAX);
    // SAFETY: `CMTime::new` is a pure value construction (sets the `Valid` flag);
    // it touches no pointers or Objective-C objects.
    unsafe { CMTime::new(1, timescale) }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::super::Frame;
    use super::*;

    #[test]
    fn store_then_take_into_produces_rgb_once() {
        let mut slot = LatestFrame::default();
        slot.store(&[10, 20, 30, 255], 1, 1, 4);
        let mut last = 0u64;
        let mut out = Frame::default();
        assert!(
            slot.take_into(&mut last, &mut out),
            "first take sees new frame"
        );
        assert_eq!(out.width, 1);
        assert_eq!(out.rgb, vec![30, 20, 10]);
        assert!(!slot.take_into(&mut last, &mut out), "no new frame since");
    }

    #[test]
    fn consume_advances_without_repacking() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let mut last = 0u64;
        assert!(slot.consume(&mut last), "consume sees the stored frame");
        let mut out = Frame::default();
        assert!(
            !slot.take_into(&mut last, &mut out),
            "consume already advanced the generation"
        );
    }

    #[test]
    fn store_reuses_capacity() {
        let mut slot = LatestFrame::default();
        slot.store(&[1, 2, 3, 255], 1, 1, 4);
        let ptr = slot.bgra.as_ptr();
        slot.store(&[4, 5, 6, 255], 1, 1, 4);
        assert_eq!(slot.bgra.as_ptr(), ptr, "same size must not reallocate");
    }

    #[test]
    fn repacks_bgra_dropping_alpha_and_swapping_channels() {
        // 2x1 image, no padding. Pixel0 = B,G,R,A = 10,20,30,255 -> RGB 30,20,10.
        // Pixel1 = 40,50,60,128 -> RGB 60,50,40.
        let bgra = [10u8, 20, 30, 255, 40, 50, 60, 128];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 2, 1, &mut out);
        assert_eq!(out, vec![30, 20, 10, 60, 50, 40]);
    }

    #[test]
    fn skips_row_stride_padding() {
        // 1x2 image, bytes_per_row = 8 but width*4 = 4 (4 padding bytes/row).
        // Row0 px = 1,2,3,255 -> 3,2,1 ; padding 99,99,99,99 ignored.
        // Row1 px = 4,5,6,255 -> 6,5,4.
        let bgra = [1u8, 2, 3, 255, 99, 99, 99, 99, 4, 5, 6, 255, 88, 88, 88, 88];
        let mut out = Vec::new();
        bgra_to_rgb(&bgra, 8, 1, 2, &mut out);
        assert_eq!(out, vec![3, 2, 1, 6, 5, 4]);
    }

    #[test]
    fn reuses_buffer_capacity() {
        let bgra = [10u8, 20, 30, 255];
        let mut out = Vec::with_capacity(3);
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        let ptr = out.as_ptr();
        bgra_to_rgb(&bgra, 4, 1, 1, &mut out);
        assert_eq!(out.as_ptr(), ptr, "same dimensions must not reallocate");
    }

    #[test]
    fn device_index_in_range_is_selected() {
        assert_eq!(select_device_index(3, 0), Some(0));
        assert_eq!(select_device_index(3, 2), Some(2));
    }

    #[test]
    fn out_of_range_index_falls_back_to_default() {
        assert_eq!(select_device_index(3, 3), None);
        assert_eq!(select_device_index(0, 0), None);
    }

    #[test]
    fn format_label_reads_like_the_nokhwa_label() {
        assert_eq!(format_label(640, 480, 30), "640x480 BGRA @30");
    }

    #[test]
    fn clamp_idle_duration_within_range_is_unchanged() {
        // A camera that supports the idle target (e.g. 1..=30 fps, i.e.
        // durations 1/30s..=1s). The 1/4s idle target sits inside the range.
        let desired = 0.25; // 1 / IDLE_INFERENCE_HZ at 4 Hz
        let got = clamp_idle_frame_duration_secs(desired, 1.0 / 30.0, 1.0);
        assert!(
            (got - desired).abs() < f64::EPSILON,
            "in-range target must pass through unchanged, got {got}"
        );
    }

    #[test]
    fn clamp_idle_duration_longer_than_max_clamps_to_max() {
        // The abort case: a fixed 30 fps camera reports a single 30..=30 range,
        // so both supported durations are 1/30s. The 1/4s idle target is LONGER
        // than the max supported duration (1/30s); an unclamped set would raise
        // an uncatchable Objective-C exception. Clamp it back down to 1/30s.
        let desired = 0.25; // 1/4s
        let max_supported = 1.0 / 30.0;
        let got = clamp_idle_frame_duration_secs(desired, max_supported, max_supported);
        assert!(
            (got - max_supported).abs() < f64::EPSILON,
            "over-max target must clamp to the max supported duration, got {got}"
        );
    }

    #[test]
    fn clamp_idle_duration_shorter_than_min_clamps_to_min() {
        // A camera whose slowest supported rate is faster than the idle target
        // direction is the over-max case above; the symmetric guard is a target
        // SHORTER than the min supported duration, which must clamp up to min.
        let desired = 1.0 / 120.0; // very short duration (120 fps)
        let min_supported = 1.0 / 30.0;
        let got = clamp_idle_frame_duration_secs(desired, min_supported, 1.0);
        assert!(
            (got - min_supported).abs() < f64::EPSILON,
            "under-min target must clamp to the min supported duration, got {got}"
        );
    }

    #[test]
    fn idle_min_frame_duration_matches_inference_cap() {
        // The idle camera rate must provably equal the worker's idle cap: a
        // 1 / IDLE_INFERENCE_HZ-second min frame duration is IDLE_INFERENCE_HZ fps.
        let t = idle_min_frame_duration();
        // `CMTime` is a packed struct; copy the Copy fields to locals before
        // asserting so we never take a reference to a misaligned field.
        let value = t.value;
        let timescale = t.timescale;
        assert_eq!(value, 1);
        assert_eq!(
            timescale,
            i32::try_from(IDLE_INFERENCE_HZ).expect("IDLE_INFERENCE_HZ fits in i32")
        );
    }

    #[test]
    #[ignore = "requires a real camera; run locally with --ignored on macOS"]
    fn opens_default_camera_and_delivers_a_frame() {
        let mut src = AvfFrameSource::open(0).expect("open default camera");
        let mut out = Frame::default();
        let mut got = false;
        for _ in 0..200 {
            if src.next_frame(&mut out).expect("frame read") {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(got, "no frame within ~2s");
        assert!(out.is_consistent() && out.width > 0);
        assert!(src.format_label().is_some());
    }
}
