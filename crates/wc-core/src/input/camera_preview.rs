//! Live camera preview in the settings dock: a low-rate, downscaled RGBA tap
//! on whichever tracking worker currently owns the webcam.
//!
//! Camera frames never normally leave the tracking worker threads (the hand
//! and body workers consume them in place; only landmarks cross to Bevy).
//! For operator framing — "what does the camera actually see?" — this module
//! adds a **tap** at the one choke point both modalities share: the
//! `FrameSource` the worker reads from. `PreviewTap` wraps the production
//! camera source; after each decoded frame it (at most `PREVIEW_MAX_HZ`
//! times per second, and only while the toggle is on) downscales the RGB
//! frame to ~`PREVIEW_TARGET_WIDTH` px, converts to RGBA, and swaps it into
//! a process-wide latest-wins slot. The settings dock reads the slot and
//! draws it as an egui texture. (Plain code spans, not intra-doc links: this
//! module doc is re-resolved at documentation sites where the bare names are
//! out of scope — the same rustdoc quirk `capture/mod.rs` documents.)
//!
//! ```text
//! tracking worker thread                     Bevy / egui (panel open only)
//! ──────────────────────                     ────────────────────────────
//! PreviewTap::next_frame                     render_preview_section
//!   └─ downscale → RGBA scratch                └─ copy_latest (seq-gated)
//!        └─ swap into ───► latest-wins slot ───► egui texture upload
//!                          (Mutex<PreviewSlot>)
//! ```
//!
//! ## Hot-path discipline
//!
//! - **Toggle off (the default): one relaxed atomic load per captured frame**
//!   and nothing else — no lock, no allocation, no copy.
//! - Toggle on: the downscale writes into a worker-owned scratch buffer that
//!   is *swapped* (not copied) into the slot, so after the first two frames
//!   both buffers are warm and the steady state allocates nothing.
//! - The slot is a `Mutex`, not a ring: contention is between one ≤10 Hz
//!   writer and a reader that only runs while the settings panel is open, and
//!   each critical section is a pointer swap / one memcpy of ~`320×240×4`
//!   bytes. (The lock-free-only rule is the *audio* thread's contract.)
//!
//! The toggle itself is `CameraPreviewSettings` (storage key
//! `camera_preview`, "Camera" section of the Display tab), mirrored into the
//! shared atomic every frame by `mirror_preview_enabled` — the same
//! unconditional-store idiom as the idle-throttle mirrors, so a worker
//! (re)built at any time observes the current value on its next frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy_egui::egui;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

use super::capture::{CaptureError, Frame, FrameSource};
use crate::settings::{RegisterDockSectionExt, RegisterSketchSettingsExt, SketchSettings};
use crate::ui::OverlayStyle;

/// Target preview width in pixels; the downscale step is chosen so the
/// published frame is at most this wide (aspect preserved by decimation).
pub const PREVIEW_TARGET_WIDTH: u32 = 320;

/// Maximum preview publish rate. 10 Hz is plenty for a framing aid and keeps
/// the added per-frame worker cost (one ~320-wide decimation) negligible next
/// to inference.
pub const PREVIEW_MAX_HZ: u32 = 10;

/// Minimum interval between preview publishes — the period of
/// [`PREVIEW_MAX_HZ`] (pinned in lockstep by a test below; spelled as a
/// literal because `u64::from` is not const-callable here).
const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Global settings toggle for the live camera preview. Works with **any**
/// webcam (it taps the tracking worker's frames, not the OBSBOT SDK), so it
/// is registered whenever a camera-consuming modality is compiled in.
#[derive(
    SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq, Eq,
)]
#[reflect(Resource, Default)]
#[settings(storage_key = "camera_preview")]
pub struct CameraPreviewSettings {
    /// Show a small live preview of the tracking camera in this panel.
    /// Default off: while off the workers skip all preview work (a single
    /// atomic check per frame).
    #[setting(
        default = false,
        ty = Boolean,
        category = User,
        section = "Camera",
        label = "Camera preview"
    )]
    #[serde(default)]
    pub camera_preview: bool,
}

/// The latest published preview frame. Guarded by the `Mutex` in
/// [`CameraPreviewShared`]; `seq` lets the reader skip untouched frames.
#[derive(Default)]
struct PreviewSlot {
    /// Preview width in pixels.
    width: u32,
    /// Preview height in pixels.
    height: u32,
    /// Publish counter; `0` means nothing has ever been published.
    seq: u64,
    /// Tightly-packed RGBA bytes (`width * height * 4`).
    rgba: Vec<u8>,
}

/// The process-wide preview channel: an enable flag (worker fast-path gate)
/// plus the latest-wins frame slot. Shared by every [`PreviewTap`] and the
/// panel reader via [`shared`]; tests construct their own instance.
pub struct CameraPreviewShared {
    /// Mirrored from [`CameraPreviewSettings::camera_preview`] each frame.
    enabled: AtomicBool,
    /// Latest published frame (see [`PreviewSlot`]).
    slot: Mutex<PreviewSlot>,
}

impl CameraPreviewShared {
    /// A fresh, disabled channel with an empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            slot: Mutex::new(PreviewSlot::default()),
        }
    }

    /// Set the enable flag (Bevy-side mirror system).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Read the enable flag (worker fast path; one relaxed load).
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Publish a frame by swapping `scratch` (RGBA bytes for a `width` ×
    /// `height` image) into the slot. On return `scratch` holds the slot's
    /// previous buffer, which the caller reuses next time — steady state is
    /// allocation-free on both sides of the swap.
    fn publish_swap(&self, scratch: &mut Vec<u8>, width: u32, height: u32) {
        // A poisoned lock (a panicked reader) silently drops the frame; the
        // preview is a diagnostic aid and must never take a worker down.
        if let Ok(mut slot) = self.slot.lock() {
            std::mem::swap(&mut slot.rgba, scratch);
            slot.width = width;
            slot.height = height;
            slot.seq = slot.seq.wrapping_add(1);
        }
    }

    /// Copy the latest frame into `out` if its sequence number differs from
    /// `last_seq`. Returns `(width, height, seq)` on a new frame, `None` when
    /// nothing new (or nothing ever) was published. `out` is reused across
    /// calls (`clear` + `extend`, capacity retained).
    pub fn copy_latest(&self, last_seq: u64, out: &mut Vec<u8>) -> Option<(u32, u32, u64)> {
        let slot = self.slot.lock().ok()?;
        if slot.seq == last_seq || slot.rgba.is_empty() {
            return None;
        }
        out.clear();
        out.extend_from_slice(&slot.rgba);
        Some((slot.width, slot.height, slot.seq))
    }
}

impl Default for CameraPreviewShared {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide shared preview channel used by production code. A
/// `LazyLock<Arc<…>>` rather than a plain static so [`PreviewTap`] can also
/// hold test-local instances by `Arc`.
static SHARED: LazyLock<Arc<CameraPreviewShared>> =
    LazyLock::new(|| Arc::new(CameraPreviewShared::new()));

/// Handle to the process-wide preview channel.
#[must_use]
pub fn shared() -> Arc<CameraPreviewShared> {
    Arc::clone(&SHARED)
}

/// A [`FrameSource`] decorator that publishes a downscaled preview of every
/// budgeted frame it passes through. Wraps the production camera source on
/// the worker thread (both the hand and the body worker), so whichever
/// modality owns the camera feeds the preview — no worker-loop changes.
pub struct PreviewTap {
    /// The real camera source.
    inner: Box<dyn FrameSource>,
    /// The channel to publish into.
    shared: Arc<CameraPreviewShared>,
    /// Reused RGBA downscale buffer (swapped with the slot on publish).
    scratch: Vec<u8>,
    /// Last publish time, for the [`PREVIEW_MAX_HZ`] cap.
    last_publish: Option<Instant>,
}

impl PreviewTap {
    /// Wrap `inner` so it publishes previews to the process-wide channel.
    #[must_use]
    pub fn wrap(inner: Box<dyn FrameSource>) -> Box<dyn FrameSource> {
        Box::new(Self::with_shared(inner, shared()))
    }

    /// Wrap `inner` publishing to an explicit channel (tests).
    #[must_use]
    pub fn with_shared(inner: Box<dyn FrameSource>, shared: Arc<CameraPreviewShared>) -> Self {
        Self {
            inner,
            shared,
            scratch: Vec::new(),
            last_publish: None,
        }
    }

    /// Publish `frame` if the toggle is on and the rate cap allows. The
    /// toggle-off fast path is a single relaxed atomic load.
    fn maybe_publish(&mut self, frame: &Frame) {
        if !self.shared.enabled() {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.last_publish {
            if now.duration_since(last) < PREVIEW_MIN_INTERVAL {
                return;
            }
        }
        if frame.width == 0 || frame.height == 0 || !frame.is_consistent() {
            return;
        }
        let step = downscale_step(frame.width, PREVIEW_TARGET_WIDTH);
        let (w, h) = downscale_rgb_to_rgba(
            &frame.rgb,
            frame.width,
            frame.height,
            step,
            &mut self.scratch,
        );
        if w == 0 || h == 0 {
            return;
        }
        self.shared.publish_swap(&mut self.scratch, w, h);
        self.last_publish = Some(now);
    }
}

impl FrameSource for PreviewTap {
    fn next_frame(&mut self, out: &mut Frame) -> Result<bool, CaptureError> {
        let got = self.inner.next_frame(out)?;
        if got {
            self.maybe_publish(out);
        }
        Ok(got)
    }

    fn discard_frame(&mut self) -> Result<bool, CaptureError> {
        // Over-budget frames are drained undecoded; there is nothing to
        // preview (and decoding one here would defeat the throttle).
        self.inner.discard_frame()
    }

    fn format_label(&self) -> Option<&str> {
        self.inner.format_label()
    }

    fn set_capture_throttle(&mut self, throttled: bool) {
        self.inner.set_capture_throttle(throttled);
    }
}

/// Integer decimation step so a `src_width`-wide frame lands at or under
/// `target_width` preview pixels: `ceil(src / target)`, min 1.
#[must_use]
pub fn downscale_step(src_width: u32, target_width: u32) -> u32 {
    src_width.div_ceil(target_width.max(1)).max(1)
}

/// Decimate a tightly-packed RGB8 frame by sampling every `step`-th pixel in
/// both axes, writing tightly-packed RGBA8 (alpha 255) into `out` (reused:
/// `clear` + `extend`, capacity retained — no steady-state allocation).
/// Returns the output `(width, height)`; `(0, 0)` — with `out` cleared — when
/// the input dimensions and byte length disagree (never panics on a torn
/// frame).
#[must_use]
pub fn downscale_rgb_to_rgba(
    rgb: &[u8],
    width: u32,
    height: u32,
    step: u32,
    out: &mut Vec<u8>,
) -> (u32, u32) {
    out.clear();
    let w = usize::try_from(width).unwrap_or(0);
    let h = usize::try_from(height).unwrap_or(0);
    let step_px = usize::try_from(step.max(1)).unwrap_or(1);
    if w == 0 || h == 0 || rgb.len() < w * h * 3 {
        return (0, 0);
    }
    for y in (0..h).step_by(step_px) {
        let row = y * w * 3;
        for x in (0..w).step_by(step_px) {
            let i = row + x * 3;
            // In bounds by the length check above: i + 3 <= w*h*3 <= rgb.len().
            out.extend_from_slice(&rgb[i..i + 3]);
            out.push(255);
        }
    }
    let step = step.max(1);
    (width.div_ceil(step), height.div_ceil(step))
}

/// Session-lived egui-side preview state: the uploaded texture, the last seen
/// sequence number, and the reused copy-out buffer. One texture, re-`set` in
/// place on each new frame — bounded by construction (mechanism 3 in the
/// GPU-resource taxonomy: an egui-managed texture freed when the handle
/// drops with this resource at app exit; it never grows with time).
#[derive(Resource, Default)]
pub struct CameraPreviewUi {
    /// The uploaded preview texture (`None` until the first frame arrives).
    texture: Option<egui::TextureHandle>,
    /// Sequence number of the frame currently uploaded.
    last_seq: u64,
    /// Reused RGBA copy-out buffer (capacity retained across frames).
    scratch: Vec<u8>,
}

/// Wires the camera preview into the [`App`]: the settings toggle, the
/// per-frame enable mirror, and the dock section that draws the image.
///
/// Signal flow: `PreUpdate` mirrors the toggle into the shared atomic (the
/// workers' fast-path gate); the tracking workers publish frames through
/// [`PreviewTap`] (installed by the camera-source constructors in
/// `input::body::systems` / `input::providers::mediapipe`); the custom dock
/// section registered after the `camera_preview` settings section pulls the
/// latest frame and draws it while the panel is open.
pub struct CameraPreviewPlugin;

impl Plugin for CameraPreviewPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<CameraPreviewSettings>()
            .init_resource::<CameraPreviewUi>()
            .add_systems(PreUpdate, mirror_preview_enabled)
            .register_dock_section(CameraPreviewSettings::STORAGE_KEY, render_preview_section);
    }
}

/// `PreUpdate`: mirror the settings toggle into the shared atomic. An
/// unconditional relaxed store every frame (the idle-throttle mirror idiom),
/// so a tracking worker (re)started at any moment observes the current value
/// on its next frame. Runs in every activity state by design; it is one
/// atomic store.
fn mirror_preview_enabled(settings: Res<'_, CameraPreviewSettings>) {
    SHARED.set_enabled(settings.camera_preview);
}

/// Custom dock section (after the `camera_preview` settings section): draw
/// the latest preview frame, or a hint while none has arrived. Renders only
/// while the settings panel is open; invisible-and-free otherwise.
fn render_preview_section(world: &mut World, ui: &mut egui::Ui, style: &OverlayStyle) {
    let enabled = world
        .get_resource::<CameraPreviewSettings>()
        .is_some_and(|s| s.camera_preview);
    if !enabled {
        return;
    }
    let Some(mut state) = world.get_resource_mut::<CameraPreviewUi>() else {
        return;
    };
    let state = &mut *state;

    // Pull the newest frame (if any) and (re)upload the texture. The scratch
    // buffer is taken out and restored so it can be borrowed alongside the
    // texture field.
    let mut scratch = std::mem::take(&mut state.scratch);
    if let Some((w, h, seq)) = SHARED.copy_latest(state.last_seq, &mut scratch) {
        let size = [
            usize::try_from(w).unwrap_or(0),
            usize::try_from(h).unwrap_or(0),
        ];
        if size[0] > 0 && size[1] > 0 && scratch.len() == size[0] * size[1] * 4 {
            state.last_seq = seq;
            // ColorImage construction + texture set are egui's own texture
            // update (it owns the pixel copy); ≤10 Hz, panel open only.
            let image = egui::ColorImage::from_rgba_unmultiplied(size, &scratch);
            match state.texture.as_mut() {
                Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                None => {
                    state.texture = Some(ui.ctx().load_texture(
                        "wc-camera-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
        }
    }
    state.scratch = scratch;

    match state.texture.as_ref() {
        Some(texture) => {
            // Fit to the panel column, never upscale past the native preview
            // width; texture.size_vec2 keeps everything in egui's own floats.
            let native = texture.size_vec2();
            let width = native.x.min(ui.available_width()).max(1.0);
            let scale = width / native.x.max(1.0);
            let display = egui::vec2(width, native.y * scale);
            ui.add(egui::Image::new(egui::load::SizedTexture::new(
                texture.id(),
                display,
            )));
        }
        None => {
            ui.label(
                egui::RichText::new(
                    "waiting for camera frames… (the preview shows the tracking camera \
                     while hand or body tracking is running)",
                )
                .size(11.0)
                .color(style.text_faint),
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;
    use crate::input::capture::MockFrameSource;

    /// Step math: at-or-under the target width, never zero.
    #[test]
    fn downscale_step_lands_at_or_under_target() {
        assert_eq!(downscale_step(1280, 320), 4);
        assert_eq!(downscale_step(640, 320), 2);
        assert_eq!(downscale_step(320, 320), 1);
        assert_eq!(downscale_step(100, 320), 1, "small frames pass through");
        assert_eq!(downscale_step(321, 320), 2, "ceil, not floor");
        assert_eq!(downscale_step(0, 320), 1, "degenerate width still steps 1");
        assert_eq!(downscale_step(640, 0), 640, "zero target clamps, no panic");
    }

    /// Stride math: a 6×4 RGB frame decimated by 2 yields a 3×2 RGBA image
    /// sampling pixels (0,0), (2,0), (4,0), (0,2), … with alpha 255.
    #[test]
    fn downscale_samples_the_expected_pixels() {
        let (w, h) = (6_u32, 4_u32);
        // Pixel (x, y) gets R = y*10 + x so samples are recognizable.
        let mut rgb = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[u8::try_from(y * 10 + x).expect("fits"), 7, 9]);
            }
        }
        let mut out = Vec::new();
        let dims = downscale_rgb_to_rgba(&rgb, w, h, 2, &mut out);
        assert_eq!(dims, (3, 2));
        assert_eq!(out.len(), 3 * 2 * 4);
        let reds: Vec<u8> = out.chunks_exact(4).map(|px| px[0]).collect();
        assert_eq!(reds, vec![0, 2, 4, 20, 22, 24]);
        assert!(out.chunks_exact(4).all(|px| px[3] == 255), "opaque alpha");
        assert!(
            out.chunks_exact(4).all(|px| px[1] == 7 && px[2] == 9),
            "G/B channels carried through"
        );
    }

    /// Non-divisible dimensions: ceil semantics on both axes (a 5×3 frame at
    /// step 2 keeps the edge samples).
    #[test]
    fn downscale_ceils_non_divisible_dimensions() {
        let rgb = vec![1_u8; 5 * 3 * 3];
        let mut out = Vec::new();
        assert_eq!(downscale_rgb_to_rgba(&rgb, 5, 3, 2, &mut out), (3, 2));
        assert_eq!(out.len(), 3 * 2 * 4);
    }

    /// A torn frame (length disagreeing with dimensions) degrades to an
    /// empty result — never a panic on the worker thread.
    #[test]
    fn downscale_tolerates_torn_frames() {
        let mut out = vec![1, 2, 3];
        assert_eq!(downscale_rgb_to_rgba(&[0; 10], 6, 4, 2, &mut out), (0, 0));
        assert!(out.is_empty(), "output cleared on a torn frame");
        assert_eq!(downscale_rgb_to_rgba(&[], 0, 0, 1, &mut out), (0, 0));
    }

    /// The reuse contract: a second downscale into the same buffer must not
    /// reallocate (capacity retained by `clear` + `extend`).
    #[test]
    fn downscale_reuses_the_output_buffer() {
        let rgb = vec![9_u8; 64 * 48 * 3];
        let mut out = Vec::new();
        let _ = downscale_rgb_to_rgba(&rgb, 64, 48, 1, &mut out);
        let cap = out.capacity();
        let ptr = out.as_ptr();
        let _ = downscale_rgb_to_rgba(&rgb, 64, 48, 1, &mut out);
        assert_eq!(out.capacity(), cap, "no regrow on same-size input");
        assert_eq!(out.as_ptr(), ptr, "same allocation reused");
    }

    /// With the toggle off, a tap passes frames through and publishes
    /// nothing — the zero-cost-when-disabled contract.
    #[test]
    fn disabled_tap_publishes_nothing() {
        let shared = Arc::new(CameraPreviewShared::new());
        let mut tap = PreviewTap::with_shared(
            Box::new(MockFrameSource::solid(8, 8, [1, 2, 3])),
            Arc::clone(&shared),
        );
        let mut out = Frame::default();
        assert!(tap.next_frame(&mut out).expect("frame passes through"));
        assert_eq!(out.width, 8, "the wrapped frame reaches the caller");
        let mut buf = Vec::new();
        assert!(
            shared.copy_latest(0, &mut buf).is_none(),
            "disabled tap must never touch the slot"
        );
    }

    /// With the toggle on, the first frame publishes and an immediate second
    /// frame is rate-capped (seq unchanged); the reader's seq gate then skips
    /// the already-seen frame.
    #[test]
    fn enabled_tap_publishes_and_rate_limits() {
        let shared = Arc::new(CameraPreviewShared::new());
        shared.set_enabled(true);
        let mut frame = Frame::default();
        frame.fit_to(8, 6);
        let mut tap = PreviewTap::with_shared(
            Box::new(MockFrameSource::looping(vec![frame])),
            Arc::clone(&shared),
        );
        let mut out = Frame::default();
        assert!(tap.next_frame(&mut out).expect("first frame"));
        let mut buf = Vec::new();
        let (w, h, seq) = shared
            .copy_latest(0, &mut buf)
            .expect("first frame published");
        assert_eq!((w, h), (8, 6), "small frames pass through undecimated");
        assert_eq!(seq, 1);
        assert_eq!(buf.len(), 8 * 6 * 4);

        // Immediately after: within PREVIEW_MIN_INTERVAL, so no new publish.
        assert!(tap.next_frame(&mut out).expect("second frame"));
        assert!(
            shared.copy_latest(seq, &mut buf).is_none(),
            "rate cap must hold the second immediate frame"
        );
    }

    /// The settings default is off and an empty (pre-feature) settings file
    /// loads off — the preview must never enable itself.
    #[test]
    fn preview_defaults_off() {
        assert!(!CameraPreviewSettings::default().camera_preview);
        let parsed: CameraPreviewSettings = toml::from_str("").expect("empty settings file loads");
        assert!(!parsed.camera_preview);
    }

    /// Pins [`PREVIEW_MIN_INTERVAL`] to [`PREVIEW_MAX_HZ`] so the two cannot
    /// drift (the interval is a literal because const `From` is unavailable).
    #[test]
    fn preview_interval_matches_max_hz() {
        assert_eq!(
            PREVIEW_MIN_INTERVAL,
            Duration::from_millis(1000 / u64::from(PREVIEW_MAX_HZ))
        );
    }

    /// Hardware preview smoke — ignored by default (needs a real webcam).
    /// Opens the production camera source (preferring an OBSBOT by name,
    /// like the body worker), wraps it in the tap, and asserts a live frame
    /// reaches the preview slot with plausible dimensions and non-blank
    /// content. Run with:
    ///
    /// ```text
    /// cargo test -p wc-core --features body-tracking-camera \
    ///     camera_preview_hardware -- --ignored --nocapture
    /// ```
    #[cfg(all(
        any(
            feature = "hand-tracking-mediapipe-camera",
            feature = "body-tracking-camera"
        ),
        not(target_os = "macos")
    ))]
    #[test]
    #[ignore = "requires a webcam; run with -- --ignored --nocapture"]
    fn camera_preview_hardware() {
        let source = crate::input::capture::NokhwaFrameSource::open(0, Some("OBSBOT"))
            .expect("open a webcam — is one attached?");
        let shared = Arc::new(CameraPreviewShared::new());
        shared.set_enabled(true);
        let mut tap = PreviewTap::with_shared(Box::new(source), Arc::clone(&shared));
        let mut frame = Frame::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut published = None;
        let mut buf = Vec::new();
        while Instant::now() < deadline && published.is_none() {
            let _ = tap.next_frame(&mut frame).expect("camera read");
            published = shared.copy_latest(0, &mut buf);
        }
        let (w, h, seq) = published.expect("no preview frame published within 5 s");
        println!(
            "preview: {w}x{h} seq={seq} src={}x{}",
            frame.width, frame.height
        );
        assert!(w > 0 && w <= PREVIEW_TARGET_WIDTH, "width {w}");
        assert!(h > 0, "height {h}");
        assert_eq!(
            buf.len(),
            usize::try_from(w * h * 4).expect("fits"),
            "tightly-packed RGBA"
        );
        // A live camera frame is not a uniform field (a lens cap would be).
        let first = &buf[0..3];
        assert!(
            buf.chunks_exact(4).any(|px| &px[0..3] != first),
            "preview content is uniform — is the lens covered?"
        );
    }

    /// `copy_latest` seq-gates: same seq → `None`; new publish → `Some`.
    #[test]
    fn copy_latest_gates_on_sequence() {
        let shared = CameraPreviewShared::new();
        let mut buf = Vec::new();
        assert!(shared.copy_latest(0, &mut buf).is_none(), "empty slot");
        let mut pixels = vec![5_u8; 2 * 2 * 4];
        shared.publish_swap(&mut pixels, 2, 2);
        let (.., seq) = shared.copy_latest(0, &mut buf).expect("new frame");
        assert!(shared.copy_latest(seq, &mut buf).is_none(), "seen frame");
    }
}
