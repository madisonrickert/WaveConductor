# Kiosk Capture Resolution + Camera Preview Panel — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the MediaPipe webcam capture resolution operator-configurable, and add a dev/ADVANCED-gated egui camera-preview panel with a hand-detection indicator, so the kiosk hand tracking can be tuned on-site with more source pixels and a live view.

**Architecture:** Three layered changes, each independently testable. (1) Thread a requested capture resolution from a new `HandTrackingSettings` field through `MediaPipeConfig` into both platform frame sources (macOS `AVCaptureSession` preset selection; nokhwa format choice). (2) Fill the gap where no camera pixels reach the Bevy world: add a `preview_enabled` atomic to the existing lock-free `MediaPipeLiveTuning`, a newest-wins shared frame slot published by the worker thread only when preview is requested, and a `CameraPreview` Bevy resource mirrored from it. (3) A new egui panel (behind the existing ADVANCED toggle) that requests the preview, uploads the frame as an egui texture each render, and overlays a "hand detected?" indicator from `TrackedHand` entities.

**Tech Stack:** Rust, Bevy 0.19, bevy_egui 0.40, `objc2`/`objc2_av_foundation` (macOS capture), `nokhwa` (non-macOS capture), ONNX Runtime (`ort`) + CoreML (unchanged here), `image` crate (`RgbImage`).

## Global Constraints

- **Bevy 0.19, bevy_egui 0.40** — both consumed via `.workspace = true`. Match the existing egui APIs: `EguiContexts::ctx_mut()` returns `Result`; texture upload via `egui::Context::load_texture` (raw-bytes → `ColorImage` → `TextureHandle`, precedent: `panel_user/template_picker.rs:81-93`).
- **Feature gating:** all camera code is under `hand-tracking-mediapipe-camera`. macOS → `AvfFrameSource`; other OSes → `NokhwaFrameSource` (`capture/mod.rs:199-208`). Inference-only paths use `hand-tracking-mediapipe`. Never add `bevy/dynamic_linking` to any `[features]` table.
- **No `unwrap()`/`expect()` in non-test code** unless the panic documents an invariant violation.
- **No per-frame allocation on the worker/hot path.** Reuse preallocated buffers: `vec.clear()` (keeps capacity), `Frame::clone_from` / slice `copy_from_slice`, `std::mem::take`. The preview publish must be zero-alloc in steady state and must cost nothing when preview is disabled.
- **No `as` numeric casts** where `From`/`TryFrom`/`u32::try_from` work.
- **Docs:** `///` on every public item, `//!` on new module roots, inline `//` for any coordinate/format contract.
- **CI gates (run before claiming done):** `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace`); `cargo doc --no-deps --workspace --document-private-items` (with `RUSTDOCFLAGS="-D warnings"`); `cargo deny check`; `cargo xtask check-secrets`.
- **Manual smoke test:** `cargo rund` (fast dynamic-linked debug). Dev-category settings are hidden until the ADVANCED toggle is flipped in the settings dock (the toggle resets each launch).
- **No developer home paths / secrets in source.**

---

## File Structure

- `crates/wc-core/src/settings/hand_tracking.rs` — add `CaptureResolution` enum + `capture_resolution` setting field. (Modify.)
- `crates/wc-core/src/input/providers/mediapipe/mod.rs` — `MediaPipeConfig` gains `capture_width`/`capture_height`; `MediaPipeProvider` gains the preview slot, `set_preview_enabled`, `latest_preview_generation`/`copy_latest_preview`; plumb resolution into `open_camera_source`. (Modify.)
- `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs` — `AvfFrameSource::open` takes width/height and maps to an `AVCaptureSession` preset with a support check. (Modify.)
- `crates/wc-core/src/input/providers/mediapipe/capture/nokhwa.rs` — `choose_camera_format` and `NokhwaFrameSource::open` take a target width/height. (Modify.)
- `crates/wc-core/src/input/providers/mediapipe/capture/mod.rs` — no signature change to the `FrameSource` trait (open is inherent per source); verify the platform `pub use`. (Reference.)
- `crates/wc-core/src/input/providers/mediapipe/pipeline.rs` — `MediaPipeLiveTuning` gains `preview_enabled: AtomicBool` + setter/getter. (Modify.)
- `crates/wc-core/src/input/providers/mediapipe/worker.rs` — publish the decoded frame into the shared preview slot when `preview_enabled`. (Modify.)
- `crates/wc-core/src/input/preview.rs` — **new** module: `CameraPreview` resource + `PreviewRequested` resource + the sync/apply systems. (Create.)
- `crates/wc-core/src/input/mod.rs` — register the preview module, resources, and systems. (Modify.)
- `crates/wc-core/src/settings/panel_user/camera_preview.rs` — **new** module: the egui preview panel + RGB→RGBA helper + detection indicator. (Create.)
- `crates/wc-core/src/settings/panel_user/mod.rs` — call the camera-preview panel, gated by the ADVANCED toggle. (Modify.)
- Tests colocated as `#[cfg(test)] mod tests` in each modified file; provider-level integration in `crates/wc-core/tests/` alongside `mediapipe_registry.rs`.

---

## Task 1: Operator-configurable capture resolution

**Files:**
- Modify: `crates/wc-core/src/settings/hand_tracking.rs` (add `CaptureResolution` + field)
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs:62-117` (`MediaPipeConfig` + `Default`), and `open_camera_source` (`mod.rs:442-462`)
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs:288-323`
- Modify: `crates/wc-core/src/input/providers/mediapipe/capture/nokhwa.rs:19-80`
- Modify: `crates/waveconductor/src/hand_providers.rs:510-522` (seed config from settings)
- Test: `#[cfg(test)] mod tests` in `hand_tracking.rs`, `mod.rs`, `nokhwa.rs`

**Interfaces:**
- Produces: `CaptureResolution` enum with `fn dimensions(self) -> (u32, u32)`; `HandTrackingSettings.capture_resolution: CaptureResolution`; `MediaPipeConfig.capture_width: u32`, `MediaPipeConfig.capture_height: u32`.
- Behaviour contract: resolution applies on the **next provider (re)start** (like `set_camera_index`/`set_mirror`, `mod.rs:236-244`). Default is `R640x480` — current behaviour is preserved until the operator opts up. The model input size is unchanged (fixed 192/224); the benefit is more source pixels for any later crop/downscale.

- [ ] **Step 1: Write the failing test for the resolution enum mapping**

Add to `crates/wc-core/src/settings/hand_tracking.rs` in `#[cfg(test)] mod tests`:

```rust
#[test]
fn capture_resolution_dimensions_map_expected() {
    use super::CaptureResolution;
    assert_eq!(CaptureResolution::R640x480.dimensions(), (640, 480));
    assert_eq!(CaptureResolution::R1280x720.dimensions(), (1280, 720));
    assert_eq!(CaptureResolution::R1920x1080.dimensions(), (1920, 1080));
    assert_eq!(CaptureResolution::default(), CaptureResolution::R640x480);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p wc-core --all-features capture_resolution_dimensions_map_expected`
Expected: FAIL — `cannot find type CaptureResolution`.

- [ ] **Step 3: Add the enum and the setting field**

In `crates/wc-core/src/settings/hand_tracking.rs`, above `HandTrackingSettings`:

```rust
/// Requested webcam capture resolution. macOS maps these to discrete
/// `AVCaptureSession` presets; other platforms bias nokhwa format selection.
/// The MediaPipe models still run at their fixed input size — a higher capture
/// resolution only provides more source pixels for downscaling / a future crop.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CaptureResolution {
    /// 640×480 — the historical default; lowest USB bandwidth and heat.
    #[default]
    R640x480,
    /// 1280×720.
    R1280x720,
    /// 1920×1080.
    R1920x1080,
}

impl CaptureResolution {
    /// The (width, height) this resolution requests from the camera.
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::R640x480 => (640, 480),
            Self::R1280x720 => (1280, 720),
            Self::R1920x1080 => (1920, 1080),
        }
    }
}
```

Add the field to `HandTrackingSettings` (after `provider`), following the existing `#[setting]`/`#[serde]` pattern:

```rust
    #[setting(
        default = CaptureResolution::R640x480,
        ty = Enum,
        category = Dev,
        section = "Hand Tracking",
        label = "Camera capture resolution"
    )]
    #[serde(default)]
    pub capture_resolution: CaptureResolution,
```

(`#[serde(default)]` uses the enum's `Default` = `R640x480`, so pre-existing config files still load.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wc-core --all-features capture_resolution_dimensions_map_expected`
Expected: PASS.

- [ ] **Step 5: Write the failing test for the config default**

Add to `crates/wc-core/src/input/providers/mediapipe/mod.rs` in `#[cfg(test)] mod tests`:

```rust
#[test]
fn mediapipe_config_default_capture_is_640x480() {
    let cfg = MediaPipeConfig::default();
    assert_eq!((cfg.capture_width, cfg.capture_height), (640, 480));
}
```

- [ ] **Step 6: Run it to verify it fails**

Run: `cargo test -p wc-core --all-features mediapipe_config_default_capture_is_640x480`
Expected: FAIL — no field `capture_width`.

- [ ] **Step 7: Add the fields to `MediaPipeConfig` and its `Default`**

In `mod.rs`, add to the `MediaPipeConfig` struct (`:62-101`):

```rust
    /// Requested capture width in pixels. The camera negotiates the nearest
    /// supported format; applies on the next provider `start`.
    pub capture_width: u32,
    /// Requested capture height in pixels. See [`Self::capture_width`].
    pub capture_height: u32,
```

In the `Default` impl (`:103-117`), add:

```rust
            capture_width: 640,
            capture_height: 480,
```

- [ ] **Step 8: Run the test to verify it passes**

Run: `cargo test -p wc-core --all-features mediapipe_config_default_capture_is_640x480`
Expected: PASS.

- [ ] **Step 9: Write the failing test for nokhwa format selection at a higher target**

Add to `crates/wc-core/src/input/providers/mediapipe/capture/nokhwa.rs` in `#[cfg(test)] mod tests` (gate with the camera feature to match the fn):

```rust
#[cfg(feature = "hand-tracking-mediapipe-camera")]
#[test]
fn choose_camera_format_prefers_target_when_available() {
    use nokhwa::utils::{CameraFormat, FrameFormat, Resolution};
    let mk = |w, h| CameraFormat::new(Resolution::new(w, h), FrameFormat::YUYV, 30);
    let formats = [mk(640, 480), mk(1280, 720), mk(1920, 1080)];
    // Target 1280x720 → the 720p format is nearest and within cap.
    let chosen = super::choose_camera_format(&formats, 1280, 720).expect("a format");
    assert_eq!((chosen.width(), chosen.height()), (1280, 720));
    // Target 640x480 → historical behaviour.
    let chosen = super::choose_camera_format(&formats, 640, 480).expect("a format");
    assert_eq!((chosen.width(), chosen.height()), (640, 480));
}
```

- [ ] **Step 10: Run it to verify it fails**

Run: `cargo test -p wc-core --all-features choose_camera_format_prefers_target_when_available`
Expected: FAIL — `choose_camera_format` takes 1 argument, not 3.

- [ ] **Step 11: Make `choose_camera_format` and `NokhwaFrameSource::open` target-aware**

In `nokhwa.rs`, change `choose_camera_format` to accept the target and derive the cap/bias from it (replacing the fixed `MAX_*`/`TARGET_AREA` use inside it; keep `MIN_*`):

```rust
#[cfg(feature = "hand-tracking-mediapipe-camera")]
fn choose_camera_format(
    formats: &[nokhwa::utils::CameraFormat],
    target_w: u32,
    target_h: u32,
) -> Option<nokhwa::utils::CameraFormat> {
    use nokhwa::utils::FrameFormat;

    fn decode_rank(format: FrameFormat) -> Option<u8> {
        match format {
            FrameFormat::YUYV | FrameFormat::RAWRGB => Some(0),
            FrameFormat::MJPEG => Some(1),
            _ => None,
        }
    }

    // Cap at the requested resolution (never negotiate larger than asked), but
    // never below the historical 640×480 ceiling floor of usefulness.
    let max_w = target_w.max(MIN_CAPTURE_W);
    let max_h = target_h.max(MIN_CAPTURE_H);
    let target_area = i64::from(target_w) * i64::from(target_h);

    formats
        .iter()
        .filter(|f| {
            decode_rank(f.format()).is_some()
                && f.width() >= MIN_CAPTURE_W
                && f.height() >= MIN_CAPTURE_H
                && f.width() <= max_w
                && f.height() <= max_h
        })
        .min_by_key(|f| {
            let rank = decode_rank(f.format()).unwrap_or(u8::MAX);
            let area = i64::from(f.width()) * i64::from(f.height());
            let area_dist = (area - target_area).abs();
            (rank, area_dist, std::cmp::Reverse(f.frame_rate()))
        })
        .copied()
}
```

Remove the now-unused `MAX_CAPTURE_W`/`MAX_CAPTURE_H`/`TARGET_AREA` consts (or keep `MIN_*` only) to avoid dead-code warnings. Update `NokhwaFrameSource::open` to take `(camera_index, target_w, target_h)` and pass them through to `choose_camera_format`.

- [ ] **Step 12: Run the test to verify it passes**

Run: `cargo test -p wc-core --all-features choose_camera_format_prefers_target_when_available`
Expected: PASS.

- [ ] **Step 13: Map resolution to an `AVCaptureSession` preset (macOS)**

In `crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`, change `AvfFrameSource::open` to accept `(camera_index: u32, width: u32, height: u32)`. Import the preset constants in the existing `objc2_av_foundation` use block (`:29-34`): `AVCaptureSessionPreset1280x720`, `AVCaptureSessionPreset1920x1080`. Replace the hardcoded preset set (`:319-323`) with:

```rust
        // SAFETY: fresh capture session.
        let session = unsafe { AVCaptureSession::new() };
        // Map the requested resolution to a discrete preset, checking support
        // and falling back to 640×480 (universally supported) if unavailable.
        // SAFETY: all three are framework `AVCaptureSessionPreset*` constants.
        let desired = unsafe {
            match (width, height) {
                (1920, 1080) => AVCaptureSessionPreset1920x1080,
                (1280, 720) => AVCaptureSessionPreset1280x720,
                _ => AVCaptureSessionPreset640x480,
            }
        };
        // SAFETY: `canSetSessionPreset:` is valid on a not-yet-running session.
        let preset = if unsafe { session.canSetSessionPreset(desired) } {
            desired
        } else {
            // SAFETY: framework constant.
            unsafe { AVCaptureSessionPreset640x480 }
        };
        // SAFETY: setting a supported preset on a not-yet-running session.
        unsafe { session.setSessionPreset(preset) };
```

- [ ] **Step 14: Plumb resolution through `open_camera_source` and seed from settings**

In `mod.rs`, change `open_camera_source(camera_index)` (`:442-462`) to `open_camera_source(camera_index, capture_width, capture_height)` and forward the dims to `AvfFrameSource::open` / `NokhwaFrameSource::open`. Update its single call site to pass `self.config.capture_width, self.config.capture_height`.

In `crates/waveconductor/src/hand_providers.rs` (`:510-522`), seed the config from the setting:

```rust
    let (capture_width, capture_height) = settings.capture_resolution.dimensions();
    let config = MediaPipeConfig {
        smoothing,
        grab_rest_deadzone: settings.grab_rest_deadzone,
        depth_calibration_k: settings.depth_calibration_k,
        smoothing_min_cutoff: settings.smoothing_min_cutoff,
        smoothing_beta: settings.smoothing_beta,
        capture_width,
        capture_height,
        ..MediaPipeConfig::default()
    };
```

- [ ] **Step 15: Run the full gate**

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings` then `cargo nextest run -p wc-core --all-features`
Expected: PASS (no dead-code/unused-import warnings; all tests green).

- [ ] **Step 16: Manual smoke test (resolution actually changes)**

Run: `cargo rund`. In the settings dock, flip **ADVANCED** on, set **Camera capture resolution → 1280×720**, then toggle the **Tracking provider** dropdown Off then back to Auto/MediaPipe (resolution applies on provider restart). Confirm in the console log that the MediaPipe `format`/`CameraFormat` label reports ~1280×720 (the diagnostic label is read back from the negotiated format at `avfoundation.rs:372-390`).
Expected: negotiated capture format reflects the higher resolution; hand tracking still works.

- [ ] **Step 17: Commit**

```bash
git add crates/wc-core/src/settings/hand_tracking.rs \
        crates/wc-core/src/input/providers/mediapipe/mod.rs \
        crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs \
        crates/wc-core/src/input/providers/mediapipe/capture/nokhwa.rs \
        crates/waveconductor/src/hand_providers.rs
git commit -F - <<'EOF'
feat(input): operator-configurable webcam capture resolution

Add a CaptureResolution setting (640x480/1280x720/1920x1080) plumbed through
MediaPipeConfig into both frame sources. macOS maps to AVCaptureSession presets
with a support-checked fallback; nokhwa biases format selection to the target.
Applies on provider restart; default 640x480 preserves prior behaviour.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 2: Publish the latest camera frame to the Bevy world (preview-gated)

**Files:**
- Modify: `crates/wc-core/src/input/providers/mediapipe/pipeline.rs:194-254` (`MediaPipeLiveTuning`)
- Modify: `crates/wc-core/src/input/providers/mediapipe/worker.rs:274` area (publish frame)
- Modify: `crates/wc-core/src/input/providers/mediapipe/mod.rs:125-160` (`MediaPipeProvider` holds slot; getters/setters)
- Create: `crates/wc-core/src/input/preview.rs` (`CameraPreview`, `PreviewRequested`, sync/apply systems)
- Modify: `crates/wc-core/src/input/mod.rs` (register module, resources, systems)
- Test: `#[cfg(test)] mod tests` in `pipeline.rs`; integration in `crates/wc-core/tests/mediapipe_preview.rs`

**Interfaces:**
- Consumes (Task 1): `MediaPipeProvider`, `MediaPipeConfig`.
- Produces:
  - `MediaPipeLiveTuning::set_preview_enabled(&self, bool)` / `preview_enabled(&self) -> bool`.
  - On `MediaPipeProvider`: `set_preview_enabled(&self, bool)`; `latest_preview_generation(&self) -> u64`; `copy_latest_preview(&self, out: &mut Frame) -> Option<u64>` (copies newest frame into `out` reusing its capacity, returns the generation, or `None` if none yet).
  - `#[derive(Resource, Default)] struct CameraPreview { pub frame: Frame, pub generation: u64 }`.
  - `#[derive(Resource, Default)] struct PreviewRequested(pub bool)`.
  - Systems `apply_preview_request` (push `PreviewRequested` → provider) and `sync_camera_preview` (provider slot → `CameraPreview`), both mirroring the downcast pattern of `apply_mediapipe_idle_throttle` (`mod.rs:340-357`).
- Contract: when `PreviewRequested(false)`, the worker does **zero** frame copies (steady-state cost is one relaxed atomic load). Publishing reuses a preallocated `Frame` in an `Arc<Mutex<PreviewSlot>>` via `clone_from` — no per-frame allocation once warmed.

- [ ] **Step 1: Write the failing test for the `preview_enabled` atomic**

Add to `pipeline.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn live_tuning_preview_enabled_defaults_off_and_toggles() {
    let t = super::MediaPipeLiveTuning::new(0.05, 0.8);
    assert!(!t.preview_enabled());
    t.set_preview_enabled(true);
    assert!(t.preview_enabled());
    t.set_preview_enabled(false);
    assert!(!t.preview_enabled());
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p wc-core --all-features live_tuning_preview_enabled_defaults_off_and_toggles`
Expected: FAIL — no method `preview_enabled`.

- [ ] **Step 3: Add the `preview_enabled` atomic to `MediaPipeLiveTuning`**

In `pipeline.rs`, add to the struct (`:196-204`):

```rust
    /// Whether the operator preview panel is open and wants frames published.
    /// Off by default so the worker copies nothing in normal operation.
    preview_enabled: AtomicBool,
```

In `MediaPipeLiveTuning::new` (`:206-213`) add `preview_enabled: AtomicBool::new(false),`. Add the accessors alongside the idle-throttle ones:

```rust
    /// Enable/disable preview-frame publishing (operator panel open).
    pub fn set_preview_enabled(&self, enabled: bool) {
        self.preview_enabled.store(enabled, Ordering::Relaxed);
    }
    /// Whether preview-frame publishing is currently requested.
    pub fn preview_enabled(&self) -> bool {
        self.preview_enabled.load(Ordering::Relaxed)
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p wc-core --all-features live_tuning_preview_enabled_defaults_off_and_toggles`
Expected: PASS.

- [ ] **Step 5: Add the shared preview slot and provider accessors**

In `mod.rs`, define near the provider:

```rust
/// Newest-wins single-slot handoff of the decoded camera frame for the operator
/// preview. Written by the worker only while preview is enabled; read on the
/// Bevy main thread. Mirrors the lock-free "latest frame" convention used by the
/// capture sources (a `Mutex` here is main-thread-only, like status/diagnostics).
#[derive(Default)]
pub(crate) struct PreviewSlot {
    frame: crate::input::providers::mediapipe::capture::Frame,
    generation: u64,
}
```

Add an `Arc<Mutex<PreviewSlot>>` field to `MediaPipeProvider` (alongside `live_tuning`), constructed in `MediaPipeProvider::new`, and clone it into the worker when spawned (same wiring as `live_tuning`). Add:

```rust
    /// Request or stop preview-frame publishing (forwards to live tuning).
    pub fn set_preview_enabled(&self, enabled: bool) {
        if let Some(t) = &self.live_tuning {
            t.set_preview_enabled(enabled);
        }
    }

    /// Copy the newest published preview frame into `out` (reusing its
    /// capacity). Returns the frame's generation, or `None` if none published.
    pub fn copy_latest_preview(
        &self,
        out: &mut crate::input::providers::mediapipe::capture::Frame,
    ) -> Option<u64> {
        let slot = self.preview_slot.lock().ok()?;
        if slot.generation == 0 {
            return None;
        }
        out.clone_from(&slot.frame);
        Some(slot.generation)
    }
```

- [ ] **Step 6: Publish the frame from the worker when enabled**

In `worker.rs`, after the frame is decoded (the reused `frame` at `:274` is populated by `next_frame`), before inference, add a gated publish. Assuming the worker holds `live_tuning: Arc<MediaPipeLiveTuning>` and `preview_slot: Arc<Mutex<PreviewSlot>>` (add the latter to the worker's owned state and thread it from `spawn_worker`):

```rust
            // Publish the decoded frame for the operator preview, newest-wins,
            // only while requested. `clone_from` reuses the slot's capacity, so
            // steady state is alloc-free; disabled state costs one atomic load.
            if live_tuning.preview_enabled() {
                if let Ok(mut slot) = preview_slot.lock() {
                    slot.frame.clone_from(&frame);
                    slot.generation = slot.generation.wrapping_add(1).max(1);
                }
            }
```

- [ ] **Step 7: Create the preview resources + systems module**

Create `crates/wc-core/src/input/preview.rs`:

```rust
//! Operator camera-preview plumbing: mirrors the MediaPipe worker's newest
//! decoded frame into a Bevy resource when the preview panel requests it, and
//! pushes that request down to the provider. Zero cost when no panel is open.

use bevy::prelude::*;

use crate::input::provider::{ProviderId, ProviderRegistry};
use crate::input::providers::mediapipe::capture::Frame;
use crate::input::providers::mediapipe::MediaPipeProvider;

/// Latest decoded camera frame, published for the operator preview panel.
#[derive(Resource, Default)]
pub struct CameraPreview {
    /// Packed RGB8 frame (`width*height*3`); empty until the first publish.
    pub frame: Frame,
    /// Monotonic generation; `0` means nothing published yet.
    pub generation: u64,
}

/// Set by the preview panel each frame it is visible; drives publishing.
#[derive(Resource, Default)]
pub struct PreviewRequested(pub bool);

/// Push the current [`PreviewRequested`] state to the MediaPipe provider.
pub fn apply_preview_request(
    requested: Res<'_, PreviewRequested>,
    mut registry: ResMut<'_, ProviderRegistry>,
) {
    for slot in registry.iter_mut() {
        if slot.id != ProviderId::MediaPipe {
            continue;
        }
        if let Some(mp) = slot
            .inner
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MediaPipeProvider>())
        {
            mp.set_preview_enabled(requested.0);
        }
    }
}

/// Mirror the provider's newest preview frame into [`CameraPreview`].
pub fn sync_camera_preview(
    requested: Res<'_, PreviewRequested>,
    mut preview: ResMut<'_, CameraPreview>,
    mut registry: ResMut<'_, ProviderRegistry>,
) {
    if !requested.0 {
        return;
    }
    for slot in registry.iter_mut() {
        if slot.id != ProviderId::MediaPipe {
            continue;
        }
        if let Some(mp) = slot
            .inner
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<MediaPipeProvider>())
        {
            if let Some(generation) = mp.copy_latest_preview(&mut preview.frame) {
                preview.generation = generation;
            }
        }
    }
}
```

Register in `crates/wc-core/src/input/mod.rs`: add `mod preview;` (and `pub use` the resources), `app.init_resource::<preview::CameraPreview>()`, `app.init_resource::<preview::PreviewRequested>()`, and add `preview::apply_preview_request` and `preview::sync_camera_preview` to the `PreUpdate` set near the existing `poll_all_providers` chain (`mod.rs:118`). Order `apply_preview_request` before `poll_all_providers` and `sync_camera_preview` after it.

- [ ] **Step 8: Write the failing integration test (mock source publishes a frame)**

Create `crates/wc-core/tests/mediapipe_preview.rs`. Use the mock frame-source fixture path (the provider supports a synthetic/mock source used by `mediapipe_registry.rs`). Drive one provider `start`+`poll` cycle with preview enabled and assert a frame is copied out:

```rust
#![cfg(feature = "hand-tracking-mediapipe")]

use wc_core::input::providers::mediapipe::capture::Frame;
use wc_core::input::providers::mediapipe::{MediaPipeConfig, MediaPipeProvider};

#[test]
fn preview_publishes_frame_only_when_enabled() {
    // Build a provider over the mock/synthetic source (see mediapipe_registry.rs
    // for the exact constructor used in tests).
    let mut provider = MediaPipeProvider::new(MediaPipeConfig::default());
    provider.start().expect("start");

    // Disabled: nothing is published.
    let mut out = Frame::default();
    // Pump a few frames.
    for _ in 0..5 {
        let mut msgs = Default::default();
        provider.poll(std::time::Instant::now(), &mut msgs);
    }
    assert!(provider.copy_latest_preview(&mut out).is_none());

    // Enabled: a frame is published.
    provider.set_preview_enabled(true);
    let mut published = false;
    for _ in 0..30 {
        let mut msgs = Default::default();
        provider.poll(std::time::Instant::now(), &mut msgs);
        if provider.copy_latest_preview(&mut out).is_some() {
            published = true;
            break;
        }
    }
    assert!(published, "a frame should publish once preview is enabled");
    assert_eq!(out.rgb.len(), out.expected_len());
}
```

> Note to implementer: match the exact test constructor/mock-source wiring already used in `crates/wc-core/tests/mediapipe_registry.rs`. If the mock source requires an explicit builder (not `MediaPipeConfig::default()`), reuse that fixture verbatim rather than inventing one.

- [ ] **Step 9: Run it to verify it fails, then passes after wiring**

Run: `cargo test -p wc-core --all-features preview_publishes_frame_only_when_enabled`
Expected: first FAIL (compile/None), then PASS after Steps 5-7 wiring is complete and the mock-source fixture is matched.

- [ ] **Step 10: Run the full gate**

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings` then `cargo nextest run -p wc-core --all-features`
Expected: PASS.

- [ ] **Step 11: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/pipeline.rs \
        crates/wc-core/src/input/providers/mediapipe/worker.rs \
        crates/wc-core/src/input/providers/mediapipe/mod.rs \
        crates/wc-core/src/input/preview.rs \
        crates/wc-core/src/input/mod.rs \
        crates/wc-core/tests/mediapipe_preview.rs
git commit -F - <<'EOF'
feat(input): publish decoded camera frame to a Bevy resource for preview

Add a preview_enabled atomic to MediaPipeLiveTuning, a newest-wins preview slot
the worker fills only while enabled (alloc-free via clone_from), and a
CameraPreview resource mirrored by a PreUpdate system. Zero worker cost when no
preview panel is open.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Task 3: Camera preview egui panel (ADVANCED-gated) + detection indicator

**Files:**
- Create: `crates/wc-core/src/settings/panel_user/camera_preview.rs`
- Modify: `crates/wc-core/src/settings/panel_user/mod.rs` (call the panel, ADVANCED-gated; set `PreviewRequested`)
- Test: `#[cfg(test)] mod tests` in `camera_preview.rs` (the pure RGB→RGBA helper)

**Interfaces:**
- Consumes (Task 2): `CameraPreview` resource, `PreviewRequested` resource.
- Consumes (existing): `TrackedHand` marker component (`crates/wc-core/src/input/entity.rs`) for the detection count; the ADVANCED flag `SettingsDockAdvanced` (`panel_user/dock.rs:63`); egui texture upload precedent (`template_picker.rs:81-93`).
- Produces: `fn rgb_to_color_image(frame: &Frame) -> Option<egui::ColorImage>`; a render entry point `pub(super) fn draw_camera_preview(world: &mut World, ui: &mut egui::Ui, texture: &mut Option<egui::TextureHandle>)`.
- Contract: while the panel is drawn it sets `PreviewRequested(true)`; when the ADVANCED toggle is off (panel not drawn) a paired step sets `PreviewRequested(false)` so publishing stops. The texture is re-uploaded each render only when `CameraPreview.generation` changed.

- [ ] **Step 1: Write the failing test for the RGB→RGBA conversion**

Create `crates/wc-core/src/settings/panel_user/camera_preview.rs` with a test first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::providers::mediapipe::capture::Frame;

    #[test]
    fn rgb_to_color_image_expands_alpha_and_dims() {
        let frame = Frame { width: 2, height: 1, rgb: vec![10, 20, 30, 40, 50, 60] };
        let img = rgb_to_color_image(&frame).expect("image");
        assert_eq!(img.size, [2, 1]);
        // Two opaque RGBA pixels.
        assert_eq!(img.as_raw(), &[10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn rgb_to_color_image_rejects_inconsistent_frame() {
        let frame = Frame { width: 4, height: 4, rgb: vec![0, 0, 0] };
        assert!(rgb_to_color_image(&frame).is_none());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p wc-core --all-features rgb_to_color_image`
Expected: FAIL — `rgb_to_color_image` not found.

- [ ] **Step 3: Implement the module + conversion helper**

Write the module body of `camera_preview.rs`:

```rust
//! Operator camera-preview panel: shows the latest decoded webcam frame as an
//! egui texture with a hand-detection indicator, so exposure/framing can be
//! judged on-site. Dev/ADVANCED-gated; requests publishing only while visible.

use bevy::prelude::*;
use bevy_egui::egui;

use crate::input::entity::TrackedHand;
use crate::input::preview::{CameraPreview, PreviewRequested};
use crate::input::providers::mediapipe::capture::Frame;

/// Convert a packed RGB8 [`Frame`] into an opaque egui [`ColorImage`].
/// Returns `None` if the frame is empty or its buffer length is inconsistent.
pub(super) fn rgb_to_color_image(frame: &Frame) -> Option<egui::ColorImage> {
    if frame.width == 0 || frame.height == 0 || frame.rgb.len() != frame.expected_len() {
        return None;
    }
    let w = usize::try_from(frame.width).ok()?;
    let h = usize::try_from(frame.height).ok()?;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for px in frame.rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --all-features rgb_to_color_image`
Expected: PASS (both cases).

- [ ] **Step 5: Add the render entry point**

Append to `camera_preview.rs`:

```rust
/// Draw the preview into `ui`, re-uploading the texture only when the frame
/// generation changed. `texture` is retained across frames by the caller.
pub(super) fn draw_camera_preview(
    world: &mut World,
    ui: &mut egui::Ui,
    texture: &mut Option<egui::TextureHandle>,
    last_generation: &mut u64,
) {
    // Detection indicator from live TrackedHand entities.
    let hand_count = world.query_filtered::<(), With<TrackedHand>>().iter(world).count();

    let preview = world.resource::<CameraPreview>();
    if preview.generation != 0 && preview.generation != *last_generation {
        if let Some(img) = rgb_to_color_image(&preview.frame) {
            let handle = ui.ctx().load_texture(
                "wc-camera-preview",
                img,
                egui::TextureOptions::LINEAR,
            );
            *texture = Some(handle);
            *last_generation = preview.generation;
        }
    }

    if let Some(tex) = texture.as_ref() {
        // Fit to a reasonable panel width, preserve aspect.
        let size = tex.size_vec2();
        let max_w = 320.0;
        let scale = (max_w / size.x).min(1.0);
        ui.image((tex.id(), size * scale));
    } else {
        ui.label("Waiting for camera frame…");
    }

    if hand_count > 0 {
        ui.colored_label(egui::Color32::from_rgb(0x3c, 0xb3, 0x71), format!("● hand detected ({hand_count})"));
    } else {
        ui.colored_label(egui::Color32::GRAY, "○ no hand");
    }
}
```

> Note to implementer: `ui.image((TextureId, Vec2))` and `load_texture` signatures are the egui 0.31-era API paired with bevy_egui 0.40; if the exact `Image`/`image` call differs in this egui version, match the form already compiling in `template_picker.rs` (which uses `ctx.load_texture` and displays via the same egui version). Do not change egui/bevy_egui versions.

- [ ] **Step 6: Wire the panel into the dock, ADVANCED-gated**

In `panel_user/mod.rs`, inside `draw_user_panel`, after reading the `advanced` flag (`:176-178`): when `advanced` is true, add a collapsing "Camera preview" section to the dock that calls `camera_preview::draw_camera_preview(world, ui, &mut retained_texture, &mut last_gen)` and set `PreviewRequested(true)`; otherwise set `PreviewRequested(false)`. Retain the `Option<egui::TextureHandle>` and `last_generation: u64` across frames (store them in a `#[derive(Resource, Default)]` local resource `CameraPreviewUiState`, or a `Local` on the system — a `Local` is simplest since `draw_user_panel` is an exclusive system; if `&mut World` prevents a `Local`, add a small resource). Concretely, set the request each frame:

```rust
    world.resource_mut::<crate::input::preview::PreviewRequested>().0 = advanced;
```

Add `mod camera_preview;` to `panel_user/mod.rs`.

- [ ] **Step 7: Run the gate**

Run: `cargo clippy --all-targets --all-features --workspace -- -D warnings` then `cargo nextest run -p wc-core --all-features` and `cargo test --doc -p wc-core --all-features`
Expected: PASS.

- [ ] **Step 8: Manual smoke test (the instrument works)**

Run: `cargo rund`. Flip **ADVANCED** on in the settings dock. Confirm: (a) a "Camera preview" section appears showing the live webcam image; (b) putting a hand in view flips the indicator to green "● hand detected (1)"; (c) turning ADVANCED off makes the preview disappear and (via logs or a CPU check) the worker stops publishing. Optionally raise **Camera capture resolution** (Task 1) and confirm the preview image sharpens after a provider restart.
Expected: live preview + working indicator, no stutter in tracking.

- [ ] **Step 9: Commit**

```bash
git add crates/wc-core/src/settings/panel_user/camera_preview.rs \
        crates/wc-core/src/settings/panel_user/mod.rs
git commit -F - <<'EOF'
feat(settings): ADVANCED-gated camera preview panel with hand indicator

Show the latest decoded webcam frame as an egui texture (re-uploaded only on
generation change) plus a TrackedHand-count detection indicator. Requests frame
publishing only while the panel is visible.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Out of scope / follow-ups (deliberately deferred)

- **Adaptive interaction-zone crop (A3 crop).** Insertion point identified (`pipeline.rs:483-495`, before `square_pad_into`, with a crop-aware `ContentRect`), and the coordinate mapping back to full-frame normalized space is the trickiest correctness surface. Deferred until the preview panel exists so the operator can judge whether hands are actually too small after the resolution bump — avoids sacrificing FOV prematurely. Its own plan.
- **Crop placement UI + draggable rect overlay** on the preview — pairs with the crop task.
- **Auto-restart provider on `capture_resolution` change.** For now resolution applies on the next provider (re)start (toggle the provider dropdown). A `restart_on_*_settings_change`-style listener could apply it live; needs the exact existing restart-listener API (not captured here).
- **Built-in-cam exposure experiment** (AVFoundation custom exposure on the built-in M1 webcam) — a separate throwaway spike to validate the exposure hypothesis with zero purchase; not a durable code change.
- **Full-vs-Lite landmark model check** — verify `assets/models/hand/hand_landmark.onnx` is the Full tier; a possible free accuracy win, but needs asset sourcing, not code.
- **Out-of-band IOKit UVC exposure control (A1)** — only on the fallback plain-webcam path; gated on the OBSBOT decision (deferred pending SDK access).

## Self-review notes

- Spec coverage: A3 (resolution) = Task 1; A1b (preview + indicator) = Tasks 2-3; A3 (crop) explicitly deferred with rationale; A1/A2/A4/A5 correctly out of scope per the current decision state.
- Type consistency: `CaptureResolution::dimensions -> (u32,u32)`; `MediaPipeConfig.capture_width/height: u32`; `copy_latest_preview(&mut Frame) -> Option<u64>`; `CameraPreview{frame: Frame, generation: u64}`; `PreviewRequested(bool)`; `rgb_to_color_image(&Frame) -> Option<egui::ColorImage>` — names used consistently across tasks.
- Known implementer discretion points (flagged inline, not placeholders): the exact mock-source constructor in the Task 2 integration test (match `mediapipe_registry.rs`), and the exact egui image-display call form (match `template_picker.rs`). Both reference a concrete existing compiling site rather than guessing.
