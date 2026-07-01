//! Webcam frame capture behind a [`FrameSource`] trait.
//!
//! Abstracting capture lets the pipeline and tests run without a physical
//! camera: tests inject a [`MockFrameSource`], while the production backend is
//! selected per platform — `AvfFrameSource` on macOS (`AVFoundation` via
//! `objc2`), `NokhwaFrameSource` on Linux and Windows (`nokhwa`). Both backends
//! are gated on the `hand-tracking-mediapipe-camera` feature. Frames are written into a
//! caller-owned, reused [`Frame`] buffer so the worker performs no per-frame
//! heap allocation after warm-up.
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

    /// Hint that the app entered (`true`) or left (`false`) the idle/screensaver
    /// throttle. Backends that can lower the *hardware* capture rate do so here,
    /// shedding sensor/ISP work beyond the worker's decode-skipping. Called by
    /// the worker only on transitions (edge-triggered), never per frame.
    ///
    /// Default: no-op. Implemented by `AvfFrameSource` on macOS; a documented
    /// follow-up for the nokhwa V4L2/MediaFoundation backends.
    fn set_capture_throttle(&mut self, _throttled: bool) {}
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

/// Production webcam backend, selected per platform.
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
mod nokhwa;

#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
mod avfoundation;
#[cfg(all(feature = "hand-tracking-mediapipe-camera", target_os = "macos"))]
pub use avfoundation::AvfFrameSource;
#[cfg(all(feature = "hand-tracking-mediapipe-camera", not(target_os = "macos")))]
pub use nokhwa::NokhwaFrameSource;

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
