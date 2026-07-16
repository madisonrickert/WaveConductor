//! Nokhwa webcam backend (`AVFoundation` / `V4L2` / `MediaFoundation`).
//!
//! Compiled behind the `hand-tracking-mediapipe-camera` feature; selected by
//! `capture/mod.rs` as the production [`super::FrameSource`] implementation
//! until a platform-specific backend supersedes it on a given target.

use super::{CaptureError, Frame, FrameSource};

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

/// Enumerate the available capture devices as `(index, human_name)` pairs.
///
/// Queries the platform capture backend (Media Foundation on Windows, the
/// nokhwa auto-backend elsewhere). Returns an empty list on any query failure,
/// which the caller treats as "fall back to the configured index." Only the
/// integer-indexed devices are kept — string-addressed backends (IP cameras)
/// are not a webcam-selection target here.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
fn enumerate_devices() -> Vec<(u32, String)> {
    use nokhwa::utils::{ApiBackend, CameraIndex};

    #[cfg(target_os = "windows")]
    let backend = ApiBackend::MediaFoundation;
    #[cfg(not(target_os = "windows"))]
    let backend = ApiBackend::Auto;

    match nokhwa::query(backend) {
        Ok(list) => list
            .iter()
            .filter_map(|info| match info.index() {
                CameraIndex::Index(i) => Some((*i, info.human_name())),
                CameraIndex::String(_) => None,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Choose the device index whose human-readable name contains `want`
/// (case-insensitive substring), else `fallback`.
///
/// Pure and hardware-free so device selection is unit-tested. First match wins;
/// `None`/no-match/empty-list all return `fallback`, preserving pure index
/// behavior when no name is configured.
#[cfg(feature = "hand-tracking-mediapipe-camera")]
fn match_camera_by_name(devices: &[(u32, String)], want: Option<&str>, fallback: u32) -> u32 {
    let Some(want) = want else {
        return fallback;
    };
    let want = want.to_lowercase();
    devices
        .iter()
        .find(|(_, name)| name.to_lowercase().contains(&want))
        .map_or(fallback, |(index, _)| *index)
}

#[cfg(feature = "hand-tracking-mediapipe-camera")]
impl NokhwaFrameSource {
    /// Select a device (by `camera_name` if given, else `camera_index`), narrow
    /// to the cheapest usable enumerated format, and start streaming.
    ///
    /// Device selection first: enumerating and matching `camera_name` as a
    /// case-insensitive substring lets the app bind to a specific camera (e.g.
    /// the `OBSBot`) even when the OS does not place it at index 0 — on Windows,
    /// MSMF also enumerates a "Windows Virtual Camera Device" and an RDP camera
    /// bus, so a bare index is a gamble. Enumeration degrades gracefully to
    /// `camera_index` on any query failure or no match, and the resolved roster
    /// is logged for field diagnostics. Runs once at start — not a hot path.
    ///
    /// Then opens at `AbsoluteHighestFrameRate` first (the request that reliably
    /// opens across `V4L2`/`AVFoundation`/`MSMF`), queries the device's
    /// enumerated formats and switches to the one [`choose_camera_format`] picks.
    /// Both the format enumeration and switch degrade gracefully: any failure
    /// leaves the camera on the format it already opened with.
    ///
    /// # Errors
    /// Returns [`CaptureError::NoCamera`] if the device cannot be opened.
    pub fn open(camera_index: u32, camera_name: Option<&str>) -> Result<Self, CaptureError> {
        use nokhwa::pixel_format::RgbFormat;
        use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
        use nokhwa::Camera;

        // Resolve the target device and log the roster before opening.
        let devices = enumerate_devices();
        let index_num = match_camera_by_name(&devices, camera_name, camera_index);
        if devices.is_empty() {
            tracing::info!(
                opening_index = index_num,
                "webcam: device enumeration returned no cameras; opening configured index"
            );
        } else {
            let roster = devices
                .iter()
                .map(|(i, name)| format!("[{i}] {name}"))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!(
                cameras = %roster,
                requested_name = ?camera_name,
                opening_index = index_num,
                "webcam: enumerated capture devices"
            );
        }

        let index = CameraIndex::Index(index_num);
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

    fn devices() -> Vec<(u32, String)> {
        vec![
            (0, "Windows Virtual Camera Device".to_string()),
            (1, "OBSBOT Tiny 2 Lite StreamCamera".to_string()),
            (2, "Integrated Webcam".to_string()),
        ]
    }

    #[test]
    fn name_match_selects_device_not_at_index_zero() {
        // The OBSBot is enumerated at index 1, not 0 (index 0 is a virtual cam).
        assert_eq!(match_camera_by_name(&devices(), Some("OBSBOT"), 0), 1);
    }

    #[test]
    fn name_match_is_case_insensitive_substring() {
        assert_eq!(match_camera_by_name(&devices(), Some("obsbot tiny"), 0), 1);
    }

    #[test]
    fn no_name_match_falls_back_to_configured_index() {
        assert_eq!(match_camera_by_name(&devices(), Some("Logitech"), 2), 2);
    }

    #[test]
    fn none_name_uses_fallback_index() {
        assert_eq!(match_camera_by_name(&devices(), None, 2), 2);
    }

    #[test]
    fn empty_enumeration_uses_fallback_index() {
        assert_eq!(match_camera_by_name(&[], Some("OBSBOT"), 0), 0);
    }
}
