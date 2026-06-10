//! Webcam frame capture behind a [`FrameSource`] trait.
//!
//! Abstracting capture lets the pipeline and tests run without a physical
//! camera: tests inject a [`MockFrameSource`], while the production
//! `NokhwaFrameSource` (behind the `hand-tracking-mediapipe-camera` feature,
//! added with the worker in a later phase) wraps a real webcam. Frames are
//! written into a caller-owned, reused [`Frame`] buffer so the worker performs
//! no per-frame heap allocation after warm-up.
//!
//! Foundation module: consumed by the worker (plan Phase 8); exercised by tests
//! until then.
#![allow(dead_code)]

use thiserror::Error;

/// An error acquiring a frame from a source.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// No camera matched the requested index / none is attached.
    #[error("no camera available: {0}")]
    NoCamera(String),
    /// The camera was opened but a frame read failed.
    #[error("frame read failed: {0}")]
    Read(String),
}

/// A single captured frame: tightly-packed RGB8, row-major, top-left origin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// `width * height * 3` bytes, R,G,B per pixel.
    pub rgb: Vec<u8>,
}

impl Frame {
    /// Expected byte length for the current dimensions.
    #[must_use]
    pub fn expected_len(&self) -> usize {
        let w = usize::try_from(self.width).unwrap_or(0);
        let h = usize::try_from(self.height).unwrap_or(0);
        w * h * 3
    }

    /// `true` if `rgb` matches the dimensions.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.rgb.len() == self.expected_len()
    }

    /// Resize the backing buffer to match `width`×`height` (reused across
    /// frames; only reallocates when the dimensions grow).
    pub fn fit_to(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.rgb.resize(self.expected_len(), 0);
    }
}

/// A source of camera frames. Implementors write the latest frame into the
/// caller's buffer and report whether a new frame was produced.
///
/// Not `Send`: the source is created and used entirely on the worker thread
/// (via a `Send` factory), so backends with thread affinity — e.g. `nokhwa`'s
/// `AVFoundation` camera, which is `!Send` — work without crossing threads.
pub trait FrameSource {
    /// Write the next frame into `out`, returning `Ok(true)` if a new frame was
    /// produced, `Ok(false)` if none is available yet (caller should retry).
    ///
    /// # Errors
    /// Returns [`CaptureError`] if the camera is unavailable or a read fails.
    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError>;

    /// Fetch and discard the next frame **without decoding** it, returning
    /// `Ok(true)` if a frame was consumed, `Ok(false)` if none was available.
    ///
    /// The worker calls this for over-budget frames (the inference rate cap and
    /// the Idle/Screensaver throttle): draining keeps the camera stream fresh —
    /// newest frame wins, no stale-buffer build-up while throttled — while
    /// skipping the MJPEG/YUYV→RGB decode, which is the dominant per-frame CPU
    /// cost of a dropped frame and therefore most of the throttle's thermal win.
    ///
    /// **Implementation contract:** an implementation must consume the same
    /// frame from the underlying sequence that [`Self::next_frame`] would have
    /// consumed next (sequencing parity) — a discard never skips or reorders
    /// relative to a processing call. A scripted source (e.g.
    /// [`MockFrameSource`]) advances its cursor exactly as `next_frame` would;
    /// a real camera discards the head of its capture queue.
    ///
    /// # Errors
    /// Returns [`CaptureError`] if the camera is unavailable or a read fails.
    fn discard_frame(&mut self) -> Result<bool, CaptureError>;

    /// A short human-readable label for the active capture format (e.g.
    /// `"640x480 YUYV @30"`), or `None` for sources with no meaningful format
    /// (mocks). Surfaced in provider diagnostics so the dev panel shows what the
    /// camera actually negotiated.
    fn format_label(&self) -> Option<&str> {
        None
    }
}

/// A test/replay frame source: serves a fixed list of frames, optionally
/// looping the last one so a worker keeps receiving input.
pub struct MockFrameSource {
    frames: Vec<Frame>,
    next: usize,
    loop_last: bool,
}

impl MockFrameSource {
    /// Serve `frames` once, then return `Ok(false)` forever.
    #[must_use]
    pub fn new(frames: Vec<Frame>) -> Self {
        Self {
            frames,
            next: 0,
            loop_last: false,
        }
    }

    /// Serve `frames`, then repeat the final frame indefinitely (useful for
    /// soak-style worker tests).
    #[must_use]
    pub fn looping(frames: Vec<Frame>) -> Self {
        Self {
            frames,
            next: 0,
            loop_last: true,
        }
    }

    /// A single solid-colour frame of the given size.
    #[must_use]
    pub fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Self {
        let mut frame = Frame::default();
        frame.fit_to(width, height);
        for px in frame.rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&rgb);
        }
        Self::new(vec![frame])
    }
}

impl FrameSource for MockFrameSource {
    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        let frame = if self.next < self.frames.len() {
            let f = &self.frames[self.next];
            self.next += 1;
            f
        } else if self.loop_last {
            match self.frames.last() {
                Some(f) => f,
                None => return Ok(false),
            }
        } else {
            return Ok(false);
        };
        out.fit_to(frame.width, frame.height);
        out.rgb.copy_from_slice(&frame.rgb);
        Ok(true)
    }

    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        // Mirror next_frame's sequencing (consume one queued frame, then loop
        // or run dry) so a throttled worker drains a scripted source exactly
        // like a real camera — minus the copy.
        if self.next < self.frames.len() {
            self.next += 1;
            Ok(true)
        } else if self.loop_last {
            Ok(!self.frames.is_empty())
        } else {
            Ok(false)
        }
    }
}

/// Production webcam capture via `nokhwa` (`AVFoundation` / `V4L2` / `MediaFoundation`).
/// Behind the `hand-tracking-mediapipe-camera` feature so the base build stays
/// camera-library-free and headless-testable.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
pub struct NokhwaFrameSource {
    camera: nokhwa::Camera,
    /// Human-readable label for the negotiated capture format (for diagnostics).
    format: String,
}

/// Largest capture dimensions [`choose_camera_format`] will select (720p-class):
/// bigger formats cost more USB bandwidth and decode/convert time than hand
/// tracking needs.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
const MAX_CAPTURE_W: u32 = 1280;
#[cfg(feature = "hand-tracking-mediapipe-camera")]
const MAX_CAPTURE_H: u32 = 720;
/// Smallest capture dimensions worth selecting: below this the frame is too
/// coarse for reliable landmark detection.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
const MIN_CAPTURE_W: u32 = 320;
#[cfg(feature = "hand-tracking-mediapipe-camera")]
const MIN_CAPTURE_H: u32 = 240;
/// Resolution area we bias selection toward (640×480).
#[cfg(feature = "hand-tracking-mediapipe-camera")]
const TARGET_AREA: i64 = 640 * 480;

/// Choose the cheapest usable capture format from a device's *enumerated*
/// formats.
///
/// Policy: consider only formats [`NokhwaFrameSource::next_frame`] can decode
/// (`MJPEG`, `YUYV`, `RAWRGB`) within `320×240..=1280×720`; prefer uncompressed
/// (no per-frame JPEG decode), then the resolution closest to 640×480, then a
/// higher frame rate. Returns `None` when nothing usable is in range, so the
/// caller keeps the format the camera already opened with — degrading
/// gracefully rather than requesting a blind format that may not exist (a blind
/// `Closest(640×480 MJPEG)` failed to open on `AVFoundation`).
#[cfg(feature = "hand-tracking-mediapipe-camera")]
fn choose_camera_format(
    formats: &[nokhwa::utils::CameraFormat],
) -> Option<nokhwa::utils::CameraFormat> {
    use nokhwa::utils::FrameFormat;

    // 0 = uncompressed (cheap), 1 = MJPEG (needs a JPEG decode). Formats
    // `next_frame` cannot decode return `None` and are excluded.
    fn decode_rank(format: FrameFormat) -> Option<u8> {
        match format {
            FrameFormat::YUYV | FrameFormat::RAWRGB => Some(0),
            FrameFormat::MJPEG => Some(1),
            _ => None,
        }
    }

    formats
        .iter()
        .filter(|f| {
            decode_rank(f.format()).is_some()
                && f.width() >= MIN_CAPTURE_W
                && f.height() >= MIN_CAPTURE_H
                && f.width() <= MAX_CAPTURE_W
                && f.height() <= MAX_CAPTURE_H
        })
        .min_by_key(|f| {
            let rank = decode_rank(f.format()).unwrap_or(u8::MAX);
            let area = i64::from(f.width()) * i64::from(f.height());
            let area_dist = (area - TARGET_AREA).abs();
            // Cheapest decode first, then nearest to target resolution, then the
            // highest frame rate (Reverse so larger fps sorts first).
            (rank, area_dist, std::cmp::Reverse(f.frame_rate()))
        })
        .copied()
}

#[cfg(feature = "hand-tracking-mediapipe-camera")]
impl NokhwaFrameSource {
    /// Open `camera_index`, narrow to the cheapest usable enumerated format, and
    /// start streaming.
    ///
    /// Opens at `AbsoluteHighestFrameRate` first (the request that reliably opens
    /// across `V4L2`/`AVFoundation`/`MSMF`), then queries the device's enumerated
    /// formats and switches to the one [`choose_camera_format`] picks. Both
    /// enumeration and the format switch degrade gracefully: any failure leaves
    /// the camera on the format it already opened with.
    ///
    /// # Errors
    /// Returns [`CaptureError::NoCamera`] if the device cannot be opened.
    pub fn open(camera_index: u32) -> Result<Self, CaptureError> {
        use nokhwa::pixel_format::RgbFormat;
        use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
        use nokhwa::Camera;

        let index = CameraIndex::Index(camera_index);
        let requested =
            RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let mut camera =
            Camera::new(index, requested).map_err(|e| CaptureError::NoCamera(e.to_string()))?;

        // Narrow to a cheaper enumerated format where the device offers one.
        if let Ok(formats) = camera.compatible_camera_formats() {
            if let Some(chosen) = choose_camera_format(&formats) {
                // `chosen` came from this device's enumeration, so `Closest`
                // resolves to it. A set failure is non-fatal: keep the opened
                // format.
                let request =
                    RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(chosen));
                let _ = camera.set_camera_requset(request);
            }
        }

        camera
            .open_stream()
            .map_err(|e| CaptureError::Read(e.to_string()))?;

        let active = camera.camera_format();
        let format = format!(
            "{}x{} {:?} @{}",
            active.width(),
            active.height(),
            active.format(),
            active.frame_rate()
        );
        Ok(Self { camera, format })
    }
}

#[cfg(feature = "hand-tracking-mediapipe-camera")]
impl FrameSource for NokhwaFrameSource {
    fn format_label(&self) -> Option<&str> {
        Some(&self.format)
    }

    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        use nokhwa::utils::FrameFormat;

        let buffer = self
            .camera
            .frame()
            .map_err(|e| CaptureError::Read(e.to_string()))?;
        let res = self.camera.resolution();
        let (w, h) = (res.width(), res.height());
        let raw = buffer.buffer();

        // Decode without nokhwa's `decoding` feature (which pulls the
        // IJG-licensed mozjpeg C lib): MJPEG via the pure-Rust `image` crate,
        // YUYV and raw RGB converted directly.
        match buffer.source_frame_format() {
            FrameFormat::MJPEG => {
                let img = image::load_from_memory_with_format(raw, image::ImageFormat::Jpeg)
                    .map_err(|e| CaptureError::Read(format!("MJPEG decode: {e}")))?
                    .to_rgb8();
                out.fit_to(img.width(), img.height());
                out.rgb.copy_from_slice(img.as_raw());
            }
            FrameFormat::YUYV => {
                out.fit_to(w, h);
                yuyv_to_rgb(raw, &mut out.rgb)?;
            }
            FrameFormat::RAWRGB => {
                out.fit_to(w, h);
                if raw.len() != out.rgb.len() {
                    return Err(CaptureError::Read("RAWRGB frame size mismatch".into()));
                }
                out.rgb.copy_from_slice(raw);
            }
            other => {
                return Err(CaptureError::Read(format!(
                    "unsupported camera frame format {other:?}; extend NokhwaFrameSource::next_frame"
                )));
            }
        }
        Ok(true)
    }

    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        // Pull (and drop) the newest buffer so the stream never serves a
        // throttled worker ever-staler frames, but skip the decode/convert
        // above. Residual dependency-forced cost: nokhwa's `frame()` still
        // copies the raw bytes into a `Buffer` it owns (a per-call heap
        // allocation inside nokhwa we cannot avoid without forking its API) —
        // small next to the skipped JPEG decode / YUV conversion; revisit only
        // if idle-soak profiling flags it.
        self.camera
            .frame()
            .map_err(|e| CaptureError::Read(e.to_string()))?;
        Ok(true)
    }
}

/// Convert packed YUYV (YUY2: `Y0 U Y1 V` per 2 pixels) to RGB8 in `out`.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
fn yuyv_to_rgb(yuyv: &[u8], out: &mut [u8]) -> Result<(), CaptureError> {
    if yuyv.len() / 4 * 6 != out.len() {
        return Err(CaptureError::Read("YUYV frame size mismatch".into()));
    }
    // BT.601 full-range YUV→RGB.
    let clamp = |v: f32| {
        #[allow(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "value clamped to [0,255]; float→int has no From/TryFrom"
        )]
        {
            v.clamp(0.0, 255.0).round() as u8
        }
    };
    let convert = |y: f32, u: f32, v: f32, px: &mut [u8]| {
        let c = y - 16.0;
        let d = u - 128.0;
        let e = v - 128.0;
        px[0] = clamp(1.164 * c + 1.596 * e);
        px[1] = clamp(1.164 * c - 0.392 * d - 0.813 * e);
        px[2] = clamp(1.164 * c + 2.017 * d);
    };
    for (quad, rgb6) in yuyv.chunks_exact(4).zip(out.chunks_exact_mut(6)) {
        let (y0, u, y1, v) = (
            f32::from(quad[0]),
            f32::from(quad[1]),
            f32::from(quad[2]),
            f32::from(quad[3]),
        );
        let (first, second) = rgb6.split_at_mut(3);
        convert(y0, u, v, first);
        convert(y1, u, v, second);
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn frame_fit_to_sizes_buffer() {
        let mut f = Frame::default();
        f.fit_to(4, 2);
        assert_eq!(f.rgb.len(), 4 * 2 * 3);
        assert!(f.is_consistent());
    }

    #[test]
    fn solid_source_yields_one_frame_then_stops() {
        let mut src = MockFrameSource::solid(2, 2, [10, 20, 30]);
        let mut out = Frame::default();
        assert!(src.next_frame(&mut out).expect("first frame"));
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 2);
        assert_eq!(&out.rgb[0..3], &[10, 20, 30]);
        // Only one frame, not looping.
        assert!(!src.next_frame(&mut out).expect("no second frame"));
    }

    #[test]
    fn looping_source_repeats_the_last_frame() {
        let mut a = Frame::default();
        a.fit_to(1, 1);
        a.rgb.copy_from_slice(&[1, 2, 3]);
        let mut src = MockFrameSource::looping(vec![a]);
        let mut out = Frame::default();
        assert!(src.next_frame(&mut out).expect("first frame"));
        // Keeps serving the last frame.
        assert!(src.next_frame(&mut out).expect("looped frame"));
        assert_eq!(&out.rgb[0..3], &[1, 2, 3]);
    }

    #[test]
    fn buffer_is_reused_across_frames() {
        let mut src = MockFrameSource::looping(vec![{
            let mut f = Frame::default();
            f.fit_to(3, 3);
            f
        }]);
        let mut out = Frame::default();
        src.next_frame(&mut out).expect("first frame");
        let ptr = out.rgb.as_ptr();
        // Same dimensions next frame → no reallocation.
        src.next_frame(&mut out).expect("second frame");
        assert_eq!(out.rgb.as_ptr(), ptr);
    }

    #[test]
    fn discard_frame_consumes_the_sequence_like_next_frame() {
        // The worker's over-budget path drains via discard_frame; it must
        // advance a scripted source exactly like next_frame so throttled runs
        // see the same frame ordering a real camera would deliver.
        let mut a = Frame::default();
        a.fit_to(1, 1);
        a.rgb.copy_from_slice(&[1, 2, 3]);
        let mut b = Frame::default();
        b.fit_to(1, 1);
        b.rgb.copy_from_slice(&[4, 5, 6]);

        // Non-looping: discard eats frame 0; next_frame then sees frame 1.
        let mut src = MockFrameSource::new(vec![a.clone(), b]);
        assert!(src.discard_frame().expect("discard frame 0"));
        let mut out = Frame::default();
        assert!(src.next_frame(&mut out).expect("frame 1"));
        assert_eq!(&out.rgb[0..3], &[4, 5, 6]);
        // Exhausted, not looping → both paths report no frame.
        assert!(!src.discard_frame().expect("exhausted discard"));
        assert!(!src.next_frame(&mut out).expect("exhausted next"));

        // Looping: discard keeps reporting the repeated last frame.
        let mut looped = MockFrameSource::looping(vec![a]);
        assert!(looped.discard_frame().expect("first"));
        assert!(looped.discard_frame().expect("looped"));
    }

    #[test]
    fn mock_source_has_no_format_label() {
        let src = MockFrameSource::solid(2, 2, [0, 0, 0]);
        assert_eq!(src.format_label(), None);
    }
}

#[cfg(all(test, feature = "hand-tracking-mediapipe-camera"))]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod camera_format_tests {
    use super::*;
    use nokhwa::utils::{CameraFormat, FrameFormat, Resolution};

    fn fmt(w: u32, h: u32, f: FrameFormat, fps: u32) -> CameraFormat {
        CameraFormat::new(Resolution::new(w, h), f, fps)
    }

    #[test]
    fn prefers_uncompressed_over_mjpeg_at_same_resolution() {
        let formats = vec![
            fmt(640, 480, FrameFormat::MJPEG, 30),
            fmt(640, 480, FrameFormat::YUYV, 30),
        ];
        let chosen = choose_camera_format(&formats).expect("a format in range");
        assert_eq!(
            chosen.format(),
            FrameFormat::YUYV,
            "uncompressed avoids JPEG decode"
        );
    }

    #[test]
    fn picks_resolution_closest_to_target() {
        let formats = vec![
            fmt(320, 240, FrameFormat::YUYV, 30),
            fmt(640, 480, FrameFormat::YUYV, 30),
            fmt(1280, 720, FrameFormat::YUYV, 30),
        ];
        let chosen = choose_camera_format(&formats).expect("a format in range");
        assert_eq!((chosen.width(), chosen.height()), (640, 480));
    }

    #[test]
    fn breaks_ties_on_higher_frame_rate() {
        let formats = vec![
            fmt(640, 480, FrameFormat::YUYV, 30),
            fmt(640, 480, FrameFormat::YUYV, 60),
        ];
        let chosen = choose_camera_format(&formats).expect("a format in range");
        assert_eq!(chosen.frame_rate(), 60);
    }

    #[test]
    fn excludes_undecodable_and_out_of_bounds() {
        // NV12 is undecodable by next_frame; 1920x1080 exceeds the 720p bound.
        let formats = vec![
            fmt(640, 480, FrameFormat::NV12, 30),
            fmt(1920, 1080, FrameFormat::MJPEG, 30),
        ];
        assert!(
            choose_camera_format(&formats).is_none(),
            "no decodable in-range format → keep the opened default",
        );
    }

    #[test]
    fn falls_back_to_mjpeg_when_no_uncompressed_in_range() {
        let formats = vec![
            fmt(640, 480, FrameFormat::MJPEG, 30),
            fmt(1920, 1080, FrameFormat::YUYV, 30), // out of bounds
        ];
        let chosen = choose_camera_format(&formats).expect("the bounded MJPEG");
        assert_eq!(chosen.format(), FrameFormat::MJPEG);
        assert_eq!((chosen.width(), chosen.height()), (640, 480));
    }
}
