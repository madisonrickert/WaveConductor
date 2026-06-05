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
}

/// Production webcam capture via `nokhwa` (`AVFoundation` / `V4L2` / `MediaFoundation`).
/// Behind the `hand-tracking-mediapipe-camera` feature so the base build stays
/// camera-library-free and headless-testable.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
pub struct NokhwaFrameSource {
    camera: nokhwa::Camera,
}

#[cfg(feature = "hand-tracking-mediapipe-camera")]
impl NokhwaFrameSource {
    /// Open `camera_index` and start streaming at the highest available rate.
    ///
    /// Requests `AbsoluteHighestFrameRate` and lets the backend choose the
    /// format. A resolution cap was tried (640×480 via `Closest`) to cut
    /// per-frame decode cost, but it failed to open on the macOS/AVFoundation
    /// backend — the device did not enumerate that exact format — so it was
    /// reverted. A future cap must be derived from the camera's *enumerated*
    /// formats on real hardware, not requested blind.
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
        camera
            .open_stream()
            .map_err(|e| CaptureError::Read(e.to_string()))?;
        Ok(Self { camera })
    }
}

#[cfg(feature = "hand-tracking-mediapipe-camera")]
impl FrameSource for NokhwaFrameSource {
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
}
