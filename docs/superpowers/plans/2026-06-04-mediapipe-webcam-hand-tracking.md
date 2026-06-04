# MediaPipe Webcam Hand-Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an in-process native-Rust `HandTrackingProvider` that derives 21-landmark hands from a conventional webcam using MediaPipe's two-stage ONNX models, emitting into the same Leap-device-mm coordinate convention so every existing sketch works unchanged.

**Architecture:** A dedicated worker thread owns the camera (`nokhwa`) and two `tract` ONNX sessions; it runs palm-detection → ROI → landmark, derives signals, and pushes `Hand` frames onto a lock-free `rtrb` SPSC ring. The Bevy-side `poll()` is a non-blocking drain. Inference runtime sits behind a `HandInference` trait (`tract` primary, `ort` fallback) decided by a day-one verification spike. All pre/post-processing glue (anchors, NMS, ROI affine, coordinate mapping, signals) is pure Rust with hermetic unit tests.

**Tech Stack:** Rust 1.96, Bevy 0.18, `tract-onnx` (pure-Rust inference), `nokhwa` (webcam), `rtrb` (lock-free ring), `image`, `smallvec`. Dev-only Python oracle via `uv` (`onnxruntime`, `numpy`) for the spike + golden generation.

**Reference:** Spec `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md`. Glue porting references: `WasmEdge/mediapipe-rs` (`src/tasks/vision/hand_landmark/`), `PINTO0309/hand-gesture-recognition-using-onnx`. Models: `opencv/palm_detection_mediapipe`, `opencv/handpose_estimation_mediapipe` (HuggingFace, Apache-2.0).

**Verify gates after each phase (from AGENTS.md):**
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features --workspace -- -D warnings`
- `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace` for doctests)
- Build smoke: `cargo rund` (dev) only when a visible change warrants it.

---

## Phase 0 — Verification spike (gates the runtime decision)

> Resolves spec open-questions 1–3. **Do this first.** Its recorded outputs (tensor shapes, Resize-node support, golden landmarks, handedness/world-landmark availability) parameterize Phases 3–6. Output is a written decision record committed to the repo.

### Task 0.1: Vendor the ONNX models + attribution

**Files:**
- Create: `assets/models/hand/palm_detection.onnx` (download)
- Create: `assets/models/hand/hand_landmark.onnx` (download)
- Create: `assets/models/hand/ATTRIBUTION.md`
- Create: `assets/models/hand/LICENSE`

- [ ] **Step 1: Download the two Apache-2.0 ONNX models**

```bash
mkdir -p assets/models/hand
# OpenCV Zoo MediaPipe conversions (Apache-2.0).
curl -L -o assets/models/hand/palm_detection.onnx \
  https://huggingface.co/opencv/palm_detection_mediapipe/resolve/main/palm_detection_mediapipe_2023feb.onnx
curl -L -o assets/models/hand/hand_landmark.onnx \
  https://huggingface.co/opencv/handpose_estimation_mediapipe/resolve/main/handpose_estimation_mediapipe_2023feb.onnx
ls -la assets/models/hand/
```
Expected: two files, ~2 MB and ~5 MB. (If the exact filenames 404, list the repo's `main` tree and adjust — the model card lists the canonical file.)

- [ ] **Step 2: Record attribution + license**

Write `ATTRIBUTION.md` naming both source repos, their HuggingFace URLs, the upstream MediaPipe model lineage, the Apache-2.0 license, and the SHA-256 of each file (`shasum -a 256 assets/models/hand/*.onnx`). Copy the Apache-2.0 text into `LICENSE`.

- [ ] **Step 3: Commit**

```bash
git add assets/models/hand/
git commit -m "assets: vendor MediaPipe hand ONNX models (Apache-2.0) + attribution"
```

### Task 0.2: Python oracle — dump per-stage tensors + goldens

**Files:**
- Create: `tools/handtrack-oracle/pyproject.toml`
- Create: `tools/handtrack-oracle/oracle.py`
- Create: `tools/handtrack-oracle/README.md`
- Create: `tests/fixtures/hand/sample_hand.png` (a single clear right-hand frame; can be any CC0/self-captured image — document its provenance in the README)

- [ ] **Step 1: Scaffold the uv project**

`pyproject.toml` declares `onnxruntime`, `numpy`, `opencv-python-headless`, `pillow`. README documents `uv run oracle.py ...` and states: local-only, no API spend, not shipped.

- [ ] **Step 2: Write the oracle**

`oracle.py` (run via `uv run`): loads `palm_detection.onnx`, preprocesses `sample_hand.png` to the model's input shape, runs it, and dumps to `tests/fixtures/hand/`:
- `palm_input.npy` (the preprocessed input tensor)
- `palm_output_*.npy` (each raw output tensor, named by output index/shape)
- then anchor-decodes + NMS in Python, crops/rotates the ROI, runs `hand_landmark.onnx`, and dumps `landmark_input.npy`, `landmark_output_*.npy`, and the final `landmarks_golden.npy` (21×3 in image space).
It prints a JSON manifest of every output tensor's name + shape + dtype. **This manifest is the source of truth for the tensor shapes used in Phases 3–6.**

- [ ] **Step 3: Run it and capture the manifest**

Run: `cd tools/handtrack-oracle && uv run oracle.py --image ../../tests/fixtures/hand/sample_hand.png --out ../../tests/fixtures/hand`
Expected: `.npy` files written; a printed manifest. Paste the manifest into the decision record (Task 0.4).

- [ ] **Step 4: Commit**

```bash
git add tools/handtrack-oracle/ tests/fixtures/hand/
git commit -m "tools: dev-only Python oracle for MediaPipe hand tract-vs-onnxruntime spike"
```

### Task 0.3: tract spike — load both models, diff against the oracle

**Files:**
- Modify: `xtask/Cargo.toml` (add `tract-onnx`, `ndarray`/`tract-ndarray` as dev/xtask dep behind a `spike` feature, plus `ndarray-npy` to read `.npy`)
- Create: `xtask/src/handtrack_spike.rs`
- Modify: `xtask/src/main.rs` (register `handtrack-spike` subcommand)

- [ ] **Step 1: Write the spike subcommand**

`cargo xtask handtrack-spike` loads `assets/models/hand/palm_detection.onnx` and `hand_landmark.onnx` with `tract_onnx`, reads `palm_input.npy` / `landmark_input.npy` fixtures, runs each model, and compares every output tensor against the corresponding `*_output_*.npy` from the oracle. Prints max abs error per tensor and PASS/FAIL at a `1e-3` tolerance.

- [ ] **Step 2: Run the spike**

Run: `cargo xtask handtrack-spike`
Expected outcomes:
- **PASS** → `tract` is the runtime. Record max errors.
- **Load error naming an unsupported op** (likely `Resize`) → attempt the graph-surgery fallback (Step 3). 

- [ ] **Step 3 (only if a Resize node fails): graph-surgery rewrite**

In the oracle project, add an `onnx-graphsurgeon` (or `onnx`) script that rewrites the failing dynamic `Resize` to a fixed-scale nearest resize, re-export `palm_detection.fixed.onnx`, re-run the spike against it. If it now passes, adopt the fixed model (commit it, update ATTRIBUTION). If it still fails irreparably → the runtime decision flips to `ort` (record the `ort` vendoring + NOTICE follow-on tasks in the decision record; Phase 6's `HandInference` default becomes `OrtInference`).

- [ ] **Step 4: Commit the spike tool (not gated into normal builds)**

```bash
git add xtask/
git commit -m "xtask: handtrack-spike — diff tract inference against the Python oracle"
```

### Task 0.4: Record the decision

**Files:**
- Create: `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md` *append a "Spike Results" section* (or a sibling `docs/adr/` entry if the repo prefers ADRs — check `docs/adr/`).

- [ ] **Step 1: Write the results**

Record: the oracle tensor manifest (shapes/dtypes of every model output), the tract PASS/FAIL + max errors, whether graph surgery was needed, **the final runtime choice (tract or ort)**, and answers to spec open-questions 2 (does handpose emit handedness + world landmarks?) and 3 (does nokhwa build on CI — defer to Task 1.1 if not yet known). 

- [ ] **Step 2: Commit**

```bash
git add docs/
git commit -m "docs: record MediaPipe hand-tracking spike results + runtime decision"
```

---

## Phase 1 — Foundations (feature flag, deps, provider skeleton)

> Goal: the project compiles and runs with `--features hand-tracking-mediapipe`, registers a `MediaPipeProvider` that starts in an Errored/empty state, and `WAVECONDUCTOR_HAND_PROVIDER=mediapipe` selects it. No inference yet.

### Task 1.1: Add deps + feature flag; verify nokhwa builds on Linux

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`: add `tract-onnx`, `nokhwa`)
- Modify: `crates/wc-core/Cargo.toml` (optional deps + `hand-tracking-mediapipe` feature)
- Modify: `crates/waveconductor/Cargo.toml` (`hand-tracking-mediapipe` feature fan-out)
- Modify: `crates/wc-sketches/Cargo.toml` (if needed for consumer compile)

- [ ] **Step 1: Add workspace deps (pinned)**

```toml
# [workspace.dependencies]
# Pure-Rust ONNX inference for the MediaPipe webcam provider (no native blob).
tract-onnx = "0.21"
# Cross-platform webcam capture (AVFoundation / V4L2 / MediaFoundation).
nokhwa = { version = "0.10", features = ["input-native"] }
```

- [ ] **Step 2: wc-core optional deps + feature**

In `crates/wc-core/Cargo.toml`, under the non-wasm target deps add `tract-onnx = { workspace = true, optional = true }`, `nokhwa = { workspace = true, optional = true }`, `image = { workspace = true, optional = true }`. Add to `[features]`:
```toml
# In-process MediaPipe webcam hand-tracking provider. Additive and independent
# of `hand-tracking-gestures` (which brings in leaprs). Pure-Rust (tract); no
# native blob, no extra cargo-deny SOURCES surface.
hand-tracking-mediapipe = ["dep:tract-onnx", "dep:nokhwa", "dep:image"]
```

- [ ] **Step 3: waveconductor feature fan-out**

```toml
# crates/waveconductor/Cargo.toml [features]
hand-tracking-mediapipe = [
    "wc-core/hand-tracking-mediapipe",
    "wc-sketches/hand-tracking-gestures",
]
```

- [ ] **Step 4: Verify both features compile (and nokhwa builds headless)**

Run: `cargo build -p wc-core --features hand-tracking-mediapipe`
Run: `cargo build -p waveconductor --features hand-tracking-mediapipe`
Run (CI parity / nokhwa Linux build check, if a Linux box/container is available): same build on Linux.
Expected: compiles. If `nokhwa` fails to build on Linux for want of headers, move `nokhwa` to a `hand-tracking-mediapipe-camera` sub-feature and keep inference/glue under the base feature; record this in the spike results.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/*/Cargo.toml
git commit -m "build: add hand-tracking-mediapipe feature (tract + nokhwa, additive)"
```

### Task 1.2: Provider skeleton implementing the trait

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/mod.rs`
- Modify: `crates/wc-core/src/input/providers/mod.rs` (add `pub mod mediapipe;`)
- Test: in `mediapipe/mod.rs` `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing test**

In `mediapipe/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::provider::HandTrackingProvider;
    use crate::input::state::{PrimaryState, ServiceConnection};

    #[test]
    fn provider_before_start_is_not_started() {
        let p = MediaPipeProvider::new(MediaPipeConfig::default());
        assert!(matches!(p.status().service, ServiceConnection::NotStarted));
        assert_eq!(p.status().primary(), PrimaryState::NotStarted);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe mediapipe::tests::provider_before_start_is_not_started`
Expected: FAIL — `MediaPipeProvider` undefined.

- [ ] **Step 3: Minimal implementation**

```rust
//! In-process MediaPipe webcam hand-tracking provider.
//!
//! Owns a worker thread (added in Phase 8) that runs the two-stage ONNX
//! pipeline; `poll()` non-blockingly drains completed frames from a lock-free
//! ring. Until the worker lands, this is a registration-valid skeleton that
//! reports its lifecycle through `ProviderStatus`.
#![cfg(feature = "hand-tracking-mediapipe")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::prelude::*;

use crate::input::provider::HandTrackingProvider;
use crate::input::state::{
    HandTrackingError, HandTrackingFrame, ProviderDiagnostics, ProviderStatus,
};

/// Construction-time configuration for the webcam provider.
#[derive(Debug, Clone)]
pub struct MediaPipeConfig {
    /// Camera index to open (0 = default device).
    pub camera_index: u32,
    /// Mirror the image horizontally (webcam-as-mirror).
    pub mirror: bool,
    /// Inference rate cap, Hz.
    pub max_inference_hz: u32,
}

impl Default for MediaPipeConfig {
    fn default() -> Self {
        Self { camera_index: 0, mirror: true, max_inference_hz: 30 }
    }
}

/// In-process webcam hand-tracking provider.
pub struct MediaPipeProvider {
    config: MediaPipeConfig,
    /// Shared status snapshot, written by the worker (Phase 8), read in `poll`.
    status: Arc<Mutex<ProviderStatus>>,
    diagnostics: Arc<Mutex<ProviderDiagnostics>>,
}

impl MediaPipeProvider {
    /// Construct a provider (does not open the camera; see `start`).
    #[must_use]
    pub fn new(config: MediaPipeConfig) -> Self {
        Self {
            config,
            status: Arc::new(Mutex::new(ProviderStatus::default())),
            diagnostics: Arc::new(Mutex::new(ProviderDiagnostics::default())),
        }
    }
}

impl HandTrackingProvider for MediaPipeProvider {
    fn start(&mut self) -> Result<(), HandTrackingError> {
        // Worker spawn lands in Phase 8. For now, report Errored so the
        // registry's status-check fallback behaves like a missing device.
        Err(HandTrackingError::Unavailable(
            "MediaPipeProvider worker not yet implemented".into(),
        ))
    }

    fn stop(&mut self) {}

    fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}

    fn status(&self) -> ProviderStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics.lock().map(|d| d.clone()).unwrap_or_default()
    }
}
```
Add `pub mod mediapipe;` to `providers/mod.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe mediapipe::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/
git commit -m "input/mediapipe: provider skeleton implementing HandTrackingProvider"
```

### Task 1.3: Startup selection via env var

**Files:**
- Modify: `crates/waveconductor/src/main.rs:295` (the `WAVECONDUCTOR_HAND_PROVIDER` match)

- [ ] **Step 1: Add the `mediapipe` branch**

In `install_hand_tracking_providers`, add an arm (gated `#[cfg(feature = "hand-tracking-mediapipe")]` around the body, falling through to `auto` when the feature is off):
```rust
"mediapipe" => {
    #[cfg(feature = "hand-tracking-mediapipe")]
    {
        use wc_core::input::providers::mediapipe::{MediaPipeConfig, MediaPipeProvider};
        registry.register(
            ProviderId::MediaPipe,
            ProviderRole::Primary,
            Box::new(MediaPipeProvider::new(MediaPipeConfig::default())),
        );
        let ok = registry.provider(ProviderId::MediaPipe).is_some_and(|r| {
            !matches!(
                r.inner.status().service,
                ServiceConnection::Errored | ServiceConnection::NotStarted
            )
        });
        if ok {
            tracing::info!("hand-tracking: MediaPipeProvider started");
        } else {
            tracing::warn!("hand-tracking: MediaPipeProvider failed to start; mouse/touch still work");
        }
    }
    #[cfg(not(feature = "hand-tracking-mediapipe"))]
    {
        tracing::warn!("hand-tracking: 'mediapipe' requested but feature not compiled; using auto");
        if !try_leap(&mut registry, settings.leap_background) { install_mock(&mut registry); }
    }
}
```

- [ ] **Step 2: Verify it compiles both ways**

Run: `cargo build -p waveconductor --features hand-tracking-mediapipe`
Run: `cargo build -p waveconductor` (default — feature off)
Expected: both compile.

- [ ] **Step 3: Commit**

```bash
git add crates/waveconductor/src/main.rs
git commit -m "input: select MediaPipe provider via WAVECONDUCTOR_HAND_PROVIDER=mediapipe"
```

### Task 1.4: Registry integration test

**Files:**
- Create: `crates/wc-core/tests/mediapipe_registry.rs` (mirror `tests/input_registry.rs`)

- [ ] **Step 1: Write the test**

```rust
//! MediaPipe provider registers in the ProviderRegistry and is selectable.
#![cfg(feature = "hand-tracking-mediapipe")]

use wc_core::input::provider::{ProviderId, ProviderRegistry, ProviderRole};
use wc_core::input::providers::mediapipe::{MediaPipeConfig, MediaPipeProvider};

#[test]
fn mediapipe_provider_registers_as_primary() {
    let mut reg = ProviderRegistry::default();
    reg.register(
        ProviderId::MediaPipe,
        ProviderRole::Primary,
        Box::new(MediaPipeProvider::new(MediaPipeConfig::default())),
    );
    assert!(reg.provider(ProviderId::MediaPipe).is_some());
    assert_eq!(reg.primary_id(), Some(ProviderId::MediaPipe));
}
```

- [ ] **Step 2: Run**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe mediapipe_provider_registers_as_primary`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/tests/mediapipe_registry.rs
git commit -m "test: MediaPipe provider registry integration"
```

---

## Phase 2 — Coordinate glue (`coords.rs`)

> The critical integration: map MediaPipe normalized image coords → Leap-device-mm. Pure functions, fully testable now, no models/camera needed.

### Task 2.1: Image-normalized → Leap-mm mapping

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/coords.rs`
- Modify: `mediapipe/mod.rs` (`mod coords;`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    fn approx(a: f32, b: f32) { assert!((a - b).abs() < 0.5, "{a} vs {b}"); }

    #[test]
    fn frame_left_maps_to_negative_x_mirrored() {
        // Mirror on: a hand at image-left (x=0) appears at the user's RIGHT
        // → +200 mm. (Webcam-as-mirror.)
        let p = image_norm_to_leap_mm(Vec3::new(0.0, 0.5, 0.0), /*mirror=*/ true);
        approx(p.x, 200.0);
    }

    #[test]
    fn frame_right_maps_to_positive_x_mirrored() {
        let p = image_norm_to_leap_mm(Vec3::new(1.0, 0.5, 0.0), true);
        approx(p.x, -200.0);
    }

    #[test]
    fn raising_hand_maps_toward_screen_top() {
        // image y=0 is top → height 350 mm (LEAP_Y_MAX_MM).
        let top = image_norm_to_leap_mm(Vec3::new(0.5, 0.0, 0.0), true);
        approx(top.y, 350.0);
        let bot = image_norm_to_leap_mm(Vec3::new(0.5, 1.0, 0.0), true);
        approx(bot.y, 40.0);
    }

    #[test]
    fn mirror_off_preserves_left_right() {
        let p = image_norm_to_leap_mm(Vec3::new(0.0, 0.5, 0.0), false);
        approx(p.x, -200.0);
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe coords::tests`
Expected: FAIL — `image_norm_to_leap_mm` undefined.

- [ ] **Step 3: Implement**

```rust
//! MediaPipe image-normalized coordinates → Leap-device-mm convention.
//!
//! Downstream consumers (`crate::input::projection::palm_to_world`, Line's
//! `grab^1.5 · 5^((−z+350)/160)` power model, HandMesh) were written for the
//! Leap provider, which emits palm position in **device millimetres**: x in
//! `[-200, +200]` (LEAP_X_HALFRANGE_MM), y as height-above-device in
//! `[40, 350]` (LEAP_Y_MIN_MM..LEAP_Y_MAX_MM). The MediaPipe provider maps the
//! full webcam frame into that same convention so consumers are unchanged.

use bevy::math::Vec3;

use crate::input::projection::{LEAP_X_HALFRANGE_MM, LEAP_Y_MAX_MM, LEAP_Y_MIN_MM};

/// Map a MediaPipe normalized image point (x,y in `[0,1]`, origin top-left; z is
/// the wrist-relative depth proxy already scaled to mm by the caller) into the
/// Leap-device-mm convention. `mirror` flips x so the webcam behaves as a mirror.
#[must_use]
pub fn image_norm_to_leap_mm(p: Vec3, mirror: bool) -> Vec3 {
    let x_m = if mirror { 1.0 - p.x } else { p.x };
    // [0,1] → [-HALF, +HALF]
    let x_mm = (x_m - 0.5) * (2.0 * LEAP_X_HALFRANGE_MM);
    // image y (top=0) → height mm (top=MAX): y_mm = MAX - y*(MAX-MIN)
    let y_mm = LEAP_Y_MAX_MM - p.y * (LEAP_Y_MAX_MM - LEAP_Y_MIN_MM);
    Vec3::new(x_mm, y_mm, p.z)
}
```
Ensure `LEAP_X_HALFRANGE_MM` etc. are `pub` in `projection.rs` (they already are).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe coords::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/
git commit -m "input/mediapipe: coords — image-normalized → Leap-device-mm mapping"
```

### Task 2.2: Cross-provider agreement test

**Files:**
- Test: append to `coords.rs` tests

- [ ] **Step 1: Write the test**

```rust
#[test]
fn agrees_with_leap_projection_on_a_known_pose() {
    use crate::input::projection::palm_to_world;
    use bevy::math::{Vec2, Vec3};
    let window = Vec2::new(1280.0, 720.0);
    // A palm at image-center, mirror on → (0 mm, ~195 mm). Through palm_to_world
    // that lands near screen-center, exactly as a Leap mid-range palm does.
    let mm = image_norm_to_leap_mm(Vec3::new(0.5, 0.5, 0.0), true);
    let world = palm_to_world(mm, window);
    assert!(world.x.abs() < 1.0, "x={}", world.x);
    assert!(world.y.abs() < 40.0, "y={}", world.y); // near center (slight y bias)
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe coords::tests::agrees_with_leap_projection_on_a_known_pose`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/coords.rs
git commit -m "test: MediaPipe coords agree with Leap projection on a known pose"
```

---

## Phase 3 — Palm-detection post-processing (`anchors.rs`, `palm.rs`)

> Pure math. Constants (anchor params, input size, stride list, output layout) come from the **spike manifest** (Task 0.2) and the reference `SsdAnchorsBuilder` in `WasmEdge/mediapipe-rs` (`hand_detection/builder.rs`): options `(input 192 or 256, min_scale 0.1484375, max_scale 0.75, strides [8,16,16,16], aspect 1.0)` → 2016 anchors. **Confirm these against the spike output before coding the constants.**

### Task 3.1: SSD anchor generation

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/anchors.rs`
- Create: `tests/fixtures/hand/anchors_golden.npy` (from the oracle: dump the Python-generated anchors)

- [ ] **Step 1: Add an anchor dump to the oracle, regenerate the golden**

In `oracle.py`, write the SSD anchors it uses for decoding to `anchors_golden.npy` (shape `[N,4]` = cx,cy,w,h normalized). Re-run the oracle.

- [ ] **Step 2: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_expected_anchor_count_and_matches_golden() {
        let anchors = generate_palm_anchors(&PalmAnchorOptions::mediapipe_default());
        assert_eq!(anchors.len(), 2016); // confirm against spike manifest
        // Load anchors_golden.npy and compare cx,cy,w,h within 1e-5.
        let golden = load_golden_anchors("tests/fixtures/hand/anchors_golden.npy");
        assert_eq!(anchors.len(), golden.len());
        for (a, g) in anchors.iter().zip(&golden) {
            assert!((a.cx - g.cx).abs() < 1e-5);
            assert!((a.cy - g.cy).abs() < 1e-5);
            assert!((a.w - g.w).abs() < 1e-5);
            assert!((a.h - g.h).abs() < 1e-5);
        }
    }
}
```

- [ ] **Step 3: Implement `generate_palm_anchors`**

Port the SSD anchor algorithm (the MediaPipe `SsdAnchorsCalculator` logic; reference `WasmEdge/mediapipe-rs` `hand_detection/builder.rs`). `PalmAnchorOptions::mediapipe_default()` carries the spike-confirmed constants. `Anchor { cx, cy, w, h }`. `load_golden_anchors` is a small test helper reading `.npy` (use `ndarray-npy` as a dev-dependency).

- [ ] **Step 4: Run / pass**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe anchors::tests`
Expected: PASS (count + golden match).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/anchors.rs tests/fixtures/hand/anchors_golden.npy
git commit -m "input/mediapipe: SSD palm anchor generation (golden-verified)"
```

### Task 3.2: Box decode + weighted NMS

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/palm.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_is_monotonic_and_bounded() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99 && sigmoid(-10.0) < 0.01);
    }

    #[test]
    fn nms_keeps_highest_score_and_suppresses_overlap() {
        let dets = vec![
            PalmDetection { score: 0.9, bbox: Rect::new(0.0, 0.0, 1.0, 1.0), ..Default::default() },
            PalmDetection { score: 0.8, bbox: Rect::new(0.05, 0.05, 1.0, 1.0), ..Default::default() },
            PalmDetection { score: 0.7, bbox: Rect::new(5.0, 5.0, 1.0, 1.0), ..Default::default() },
        ];
        let kept = weighted_nms(dets, 0.3);
        assert_eq!(kept.len(), 2); // one cluster near origin, one far away
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }
}
```

- [ ] **Step 2: Run / fail**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe palm::tests`
Expected: FAIL.

- [ ] **Step 3: Implement** `sigmoid`, `Rect`, `PalmDetection { score, bbox, keypoints, rotation }`, `decode_palm_boxes(raw_boxes, raw_scores, &anchors, opts) -> Vec<PalmDetection>` (anchor-relative decode + score threshold), and `weighted_nms(dets, iou_thresh)` (MediaPipe weighted/blended NMS — reference `tensors_to_detection.rs`). Keep allocation out of any per-call hot loop where feasible (the worker reuses scratch buffers; see Phase 8).

- [ ] **Step 4: Run / pass**

Run: `cargo test -p wc-core --features hand-tracking-mediapipe palm::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/palm.rs
git commit -m "input/mediapipe: palm box decode + weighted NMS"
```

---

## Phase 4 — Landmark ROI + projection (`landmark.rs`)

> ROI affine (crop+rotate from palm keypoints), de-normalize, project landmarks back to full-image coords. Reference: `WasmEdge/mediapipe-rs` `hand_landmark/mod.rs` (`NormalizedRect::from_detection` with rotation option `(90°, 0, 2)`, transform `2.6× 2.6×, -0.5 y-shift`).

### Task 4.1: ROI rect from palm detection

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/landmark.rs`

- [ ] **Step 1: Write failing tests** for `roi_from_palm(&PalmDetection) -> RoiRect` — assert the ROI is centered between the two palm keypoints, scaled by 2.6×, and rotated to align the wrist→middle-MCP vector to vertical. Use a synthetic detection with known keypoints and assert the rect center, size, and rotation angle within tolerance. (Constants from the reference.)

- [ ] **Step 2: Run / fail.** Run: `cargo test -p wc-core --features hand-tracking-mediapipe landmark::tests::roi`

- [ ] **Step 3: Implement** `RoiRect { cx, cy, w, h, rotation }` and `roi_from_palm`.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/landmark.rs
git commit -m "input/mediapipe: landmark ROI rect (crop+rotate) from palm detection"
```

### Task 4.2: De-normalize + project landmarks to image space

**Files:**
- Modify: `landmark.rs`

- [ ] **Step 1: Write failing tests** for `project_landmarks(raw_landmarks, &RoiRect, image_wh) -> [Vec3; 21]` — a landmark at ROI-center maps to the ROI center in image space; rotation/scale invert correctly. Assert with a synthetic ROI + a couple of landmarks.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** the inverse-affine projection (rotate + scale + translate the normalized landmark back into full-image normalized coords). 21-landmark array.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/landmark.rs
git commit -m "input/mediapipe: project landmark-model output back to image space"
```

---

## Phase 5 — Derived signals (`signals.rs`)

> chirality, pinch, grab, palm normal/velocity, bone_centers, and the cross-frame id tracker. Pure functions on landmark arrays.

### Task 5.1: pinch / grab / palm normal

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/signals.rs`

- [ ] **Step 1: Write failing tests**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;
    use crate::input::hand::{LandmarkIndex, LANDMARK_COUNT};

    fn pose() -> [Vec3; LANDMARK_COUNT] { /* open-hand fixture: spread landmarks */ todo!() }

    #[test]
    fn pinch_is_high_when_thumb_and_index_tips_touch() {
        let mut lm = pose();
        lm[LandmarkIndex::ThumbTip.as_index()] = Vec3::new(0.0, 0.0, 0.0);
        lm[LandmarkIndex::IndexTip.as_index()] = Vec3::new(0.0, 0.0, 0.0);
        assert!(pinch_strength(&lm) > 0.9);
    }

    #[test]
    fn grab_is_low_for_open_hand() {
        assert!(grab_strength(&pose()) < 0.2);
    }
}
```
(Replace the `pose()` `todo!()` with a concrete spread-finger landmark fixture — a `const` array of 21 Vec3s — when implementing; do not ship a `todo!()`.)

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `pinch_strength` (normalized thumb-tip↔index-tip distance vs hand scale, inverted), `grab_strength` (mean fingertip↔palm-center closure), `palm_normal` (cross product of wrist→index-MCP and wrist→pinky-MCP), each documented with the geometry. Provide the concrete open-hand fixture used by tests as a `#[cfg(test)] const`.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/signals.rs
git commit -m "input/mediapipe: derive pinch/grab/palm-normal from landmarks"
```

### Task 5.2: Cross-frame id tracker + velocity

**Files:**
- Modify: `signals.rs`

- [ ] **Step 1: Write failing tests** for `HandTracker`:
```rust
#[test]
fn tracker_keeps_id_for_nearby_same_chirality_hand() {
    let mut t = HandTracker::default();
    let id1 = t.assign(Chirality::Right, Vec3::new(0.0, 200.0, 0.0));
    let id2 = t.assign(Chirality::Right, Vec3::new(5.0, 205.0, 0.0)); // moved slightly
    assert_eq!(id1, id2);
}
#[test]
fn tracker_new_id_for_far_hand() {
    let mut t = HandTracker::default();
    let id1 = t.assign(Chirality::Right, Vec3::new(-200.0, 100.0, 0.0));
    let id2 = t.assign(Chirality::Right, Vec3::new( 200.0, 300.0, 0.0));
    assert_ne!(id1, id2);
}
```

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `HandTracker` (associates a detection to the previous frame's hand of the same chirality within a palm-distance gate; allocates a fresh `u32` id otherwise; ages out stale tracks) and `palm_velocity(prev_pos, cur_pos, dt)` (smoothed finite difference).

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/signals.rs
git commit -m "input/mediapipe: cross-frame hand id tracker + palm velocity"
```

### Task 5.3: Bone centers for HandMesh

**Files:**
- Modify: `signals.rs`

- [ ] **Step 1: Write failing test** for `bone_centers(&[Vec3;21]) -> [Vec3;20]` — each of the 20 bones (5 fingers × 4 bones) is the midpoint of its two endpoint landmarks; assert a couple of known midpoints. (Mirror `leap_native::bone_centers_from_landmarks` semantics so HandMesh renders identically.)

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `bone_centers`, matching the bone topology the Leap provider uses (read `leap_native.rs:539+` to copy the exact pairing).

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/signals.rs
git commit -m "input/mediapipe: bone centers for HandMesh (matches Leap topology)"
```

---

## Phase 6 — Inference runtime (`inference.rs`) + golden regression

> Wires `tract` behind the `HandInference` trait and proves the full Rust pipeline matches the oracle's golden landmarks on the fixture frame. Uses the runtime decided in Phase 0.

### Task 6.1: `HandInference` trait + `TractInference`

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/inference.rs`

- [ ] **Step 1: Write failing test** — load `palm_detection.onnx` via `TractInference::load`, run it on `palm_input.npy`, assert output tensor shapes equal the spike manifest's shapes.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement**
```rust
//! ONNX inference behind a runtime-agnostic trait. `tract` is the primary
//! implementation (pure Rust, single binary); an `OrtInference` can replace it
//! behind the same trait if Phase 0 selected ort. Pre/post-processing lives in
//! `palm`/`landmark`, not here, so this stays runtime-agnostic.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("model load failed: {0}")] Load(String),
    #[error("inference run failed: {0}")] Run(String),
}

/// A single tensor (row-major f32) with its shape.
pub struct Tensor { pub data: Vec<f32>, pub shape: Vec<usize> }

/// Runs one model stage.
pub trait HandInference: Send {
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError>;
}

/// tract-backed inference for one model.
pub struct TractInference { /* TypedSimplePlan<TypedModel> */ }

impl TractInference {
    pub fn load(model_bytes: &[u8], input_shape: &[usize]) -> Result<Self, InferenceError> { /* ... */ }
}

impl HandInference for TractInference {
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError> { /* ... */ }
}
```
Fill the `tract_onnx` body: `tract_onnx::onnx().model_for_read(&mut &bytes[..])`, set input fact to `input_shape`, `.into_optimized()?.into_runnable()?`, run with a tensor built from `input.data`/`input.shape`, collect outputs into `Tensor`s.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/inference.rs
git commit -m "input/mediapipe: HandInference trait + tract implementation"
```

### Task 6.2: End-to-end golden landmark regression

**Files:**
- Create: `crates/wc-core/tests/mediapipe_golden.rs`

- [ ] **Step 1: Write the test** — load both models from `assets/models/hand/`, preprocess `tests/fixtures/hand/sample_hand.png`, run the **full Rust pipeline** (palm detect → NMS → ROI → landmark → project), and assert the resulting 21 landmarks match `tests/fixtures/hand/landmarks_golden.npy` within a tolerance (e.g. `2e-2` in normalized image units — looser than the per-tensor `1e-3` to absorb ROI-resampling differences; tighten once observed).

- [ ] **Step 2: Run / fail then pass.**

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe --test mediapipe_golden`
Expected: PASS within tolerance. If it fails, the per-stage oracle `.npy`s localize which stage diverged — fix that stage, not the tolerance.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/tests/mediapipe_golden.rs
git commit -m "test: end-to-end MediaPipe pipeline matches oracle golden landmarks"
```

---

## Phase 7 — Webcam capture (`capture.rs`)

### Task 7.1: `FrameSource` trait + `MockFrameSource`

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/capture.rs`

- [ ] **Step 1: Write failing test** — `MockFrameSource::from_image(sample_hand.png)` yields one RGB `Frame` with the expected width/height and pixel buffer length.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `Frame { width, height, rgb: Vec<u8> }`, `trait FrameSource: Send { fn next_frame(&mut self, buf: &mut Frame) -> Result<bool, CaptureError>; }` (writes into a reused buffer; returns `false` if no new frame), and `MockFrameSource`.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/capture.rs
git commit -m "input/mediapipe: FrameSource trait + MockFrameSource"
```

### Task 7.2: `NokhwaFrameSource`

**Files:**
- Modify: `capture.rs`

- [ ] **Step 1: Implement** `NokhwaFrameSource::open(camera_index) -> Result<Self, CaptureError>` and `next_frame` decoding nokhwa's frame into RGB in the reused buffer (no per-frame heap alloc — pre-size the buffer on first frame). Camera presence/format errors map to `CaptureError`.

- [ ] **Step 2: Manual smoke (Madison's Mac, real webcam)** — a small `#[ignore]` test or `xtask` that opens camera 0 and prints one frame's dimensions. Run manually: `cargo test -p wc-core --features hand-tracking-mediapipe -- --ignored nokhwa_opens`. Not run in CI.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/capture.rs
git commit -m "input/mediapipe: NokhwaFrameSource webcam capture"
```

---

## Phase 8 — Pipeline + worker thread (`pipeline.rs`, `worker.rs`)

### Task 8.1: Pipeline orchestration

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/pipeline.rs`

- [ ] **Step 1: Write failing test** — `Pipeline::process(&Frame)` on the fixture frame produces a `SmallVec<[Hand; 2]>` with one hand whose `palm_position` is in the Leap-mm range and whose landmarks match the golden within tolerance. (Reuses Phase 6 inference + Phases 2–5 glue with `MockFrameSource`.)

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `Pipeline` holding two `Box<dyn HandInference>` + scratch buffers + `HandTracker`. `process`: preprocess frame → palm detect (or reuse prior ROI when a hand was tracked last frame) → NMS → ROI → landmark → project → `signals` → `coords` → assemble `Hand`. Pre-allocate all scratch (no per-frame alloc).

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/pipeline.rs
git commit -m "input/mediapipe: two-stage pipeline orchestration (ROI-reuse continuity)"
```

### Task 8.2: Worker thread + ring

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/worker.rs`

- [ ] **Step 1: Write failing test** — spawn a worker with a `MockFrameSource` (looping the fixture); assert the `rtrb` consumer receives at least one `WorkerMessage::Frame` with a hand, and that `WorkerMessage::Status` reflects `Streaming`. Use a bounded wait.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `spawn_worker(config, frame_source, pipeline) -> WorkerHandle` — an OS thread looping at the rate cap: `next_frame` → `pipeline.process` → push `WorkerMessage::Frame(SmallVec<[Hand;2]>, capture_time)` and periodic `WorkerMessage::Status(ProviderStatus)`/`Diagnostics` onto the `rtrb` producer. `WorkerHandle` holds the join handle + a stop flag (`AtomicBool`); `Drop`/`stop()` joins cleanly. Rate-cap via `std::thread::sleep` to the remaining budget.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/worker.rs
git commit -m "input/mediapipe: worker thread + lock-free rtrb ring"
```

---

## Phase 9 — Provider wiring (`mod.rs`, `status.rs`, settings)

### Task 9.1: Status mapping

**Files:**
- Create: `crates/wc-core/src/input/providers/mediapipe/status.rs`

- [ ] **Step 1: Write failing tests** — `webcam_status(WebcamState::Streaming{..})` → `service=Connected, device=Attached, health=STREAMING, streaming=Streaming{..}` → `primary() == Streaming`; `WebcamState::NoCamera` → `Errored`/`NoDevice` → `primary()` is a not-streaming variant.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** a small `WebcamState` enum + `webcam_status`/`webcam_diagnostics` builders mapping worker state onto `ProviderStatus`/`ProviderDiagnostics` per the spec's status table.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/status.rs
git commit -m "input/mediapipe: webcam ProviderStatus/Diagnostics mapping"
```

### Task 9.2: Wire `start`/`poll`/`stop` to the worker

**Files:**
- Modify: `mediapipe/mod.rs`

- [ ] **Step 1: Write failing test** — with an injected `MockFrameSource` (add `MediaPipeProvider::with_frame_source_for_test`), `start()` returns `Ok`, then a `poll()` after a brief wait writes a `HandTrackingFrame` with one hand into the `Messages` buffer; `status().primary()` is `Streaming`.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** — `start()` builds the `Pipeline` (loading models from `assets/models/hand/`, path resolved via the existing asset-path mechanism — read how other code locates `assets/`), opens the `NokhwaFrameSource` (or the test source), spawns the worker, stores the `WorkerHandle` + `rtrb` consumer + shared status `Arc`s. `poll()` drains the consumer: applies `WorkerMessage::Status`/`Diagnostics` to the shared snapshots and writes each `WorkerMessage::Frame` as a `HandTrackingFrame { provider: MediaPipe, hands, timestamp }` into `out` (provider field overwritten downstream). `stop()` signals + joins the worker. No allocation in the drain loop.

- [ ] **Step 4: Run / pass.** Also re-run Task 1.2's `not_started` test still holds before `start`.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/mod.rs
git commit -m "input/mediapipe: wire start/poll/stop to the worker + ring drain"
```

### Task 9.3: Minimal settings (camera index + mirror) via `as_any_mut`

**Files:**
- Modify: `mediapipe/mod.rs`; check `crates/wc-core/src/settings` for the pattern leaprs uses (`apply_leap_background_setting`)

- [ ] **Step 1: Write failing test** — `as_any_mut().downcast_mut::<MediaPipeProvider>()` is `Some`, and `set_mirror(false)` updates the config the worker reads.

- [ ] **Step 2: Run / fail.**

- [ ] **Step 3: Implement** `as_any_mut` returning `Some(self)`, and typed `set_mirror`/`set_camera_index` that update a shared config the worker observes (an `Arc<Mutex<MediaPipeConfig>>` or atomics). Keep scope minimal (YAGNI) — no new settings-UI in this task; the existing dev-panel section (Task 10.2) surfaces state.

- [ ] **Step 4: Run / pass.**

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/input/providers/mediapipe/mod.rs
git commit -m "input/mediapipe: typed set_mirror/set_camera_index via as_any_mut"
```

---

## Phase 10 — Integration, diagnostics, verification

### Task 10.1: Fix the stale `Hand` coordinate doc comment

**Files:**
- Modify: `crates/wc-core/src/input/hand.rs:88-91`

- [ ] **Step 1: Update the doc** — the `Hand` doc says NDC; reality is Leap-device mm (x∈[-200,200], y∈[40,350] height, z signed mm depth). Rewrite the comment to state the actual convention and reference `projection.rs`. (Per AGENTS.md: update stale comments, don't delete.)

- [ ] **Step 2: Verify rustdoc** — `cargo doc --no-deps -p wc-core`.

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/src/input/hand.rs
git commit -m "docs: correct Hand coordinate convention comment (Leap-device mm, not NDC)"
```

### Task 10.2: Dev-panel diagnostics for the MediaPipe provider

**Files:**
- Modify: wherever the Leap dev-panel diagnostics render (grep for `primary_diagnostics`/`ProviderDiagnostics` in `crates/waveconductor` UI).

- [ ] **Step 1:** Confirm the existing dev panel reads `ProviderRegistry::primary_diagnostics()` generically (provider-agnostic). If so, MediaPipe diagnostics already render — add only a manual check. If it special-cases Leap fields, add MediaPipe-relevant lines (camera name, inference latency, dropped frames).

- [ ] **Step 2: Manual** — `WAVECONDUCTOR_HAND_PROVIDER=mediapipe cargo rund`, open dev panel (`Shift+D`), confirm camera + streaming status render.

- [ ] **Step 3: Commit** (only if code changed).

### Task 10.3: Manual on-hardware verification (Madison's Mac webcam)

**Files:** none (verification task; record results in the plan/PARITY).

- [ ] **Step 1:** `WAVECONDUCTOR_HAND_PROVIDER=mediapipe cargo rund`. Enter the Line sketch. Verify: hand appears, moves the attractor across the full screen (mirror correct), grab increases attractor power, two hands track, HandMesh bones render. Status LED shows Streaming.
- [ ] **Step 2:** Note any coordinate/mirror/latency issues; file follow-ups. This is the real acceptance gate — Madison runs it.

### Task 10.4: Full CI gate pass + final cleanup

- [ ] **Step 1:** Run the full gate set with the new feature:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo deny check
cargo xtask check-secrets
```
Expected: all green. Fix any clippy (pedantic) findings in the new modules — no `unwrap`/`expect`/`panic` in non-test code, no bare `as` casts (use `TryFrom`), `///` on every public item, `//!` on every new module.

- [ ] **Step 2:** Confirm `cargo build -p waveconductor` (feature OFF) still compiles and that the default binary is unchanged.

- [ ] **Step 3: Commit** any cleanup.

```bash
git commit -am "chore: clippy/docs cleanup for mediapipe provider; full gate green"
```

### Task 10.5: Branch finish

- [ ] **Step 1:** Use the `superpowers:finishing-a-development-branch` skill to summarize the branch and prepare the merge-to-`v5-alpha` review for Madison's sign-off. Do **not** merge without her approval (per the goal directive).

---

## Self-Review

**Spec coverage:** Every spec scope item maps to a task — provider skeleton (1.2), two-stage glue (Phases 3–4, 6, 8), `HandInference` trait (6.1), `FrameSource` trait (7), worker+ring (8.2), coordinate glue (Phase 2), derived signals (Phase 5), status mapping (9.1), feature flag (1.1), startup selection (1.3), model assets (0.1), Python oracle (0.2), hermetic tests (throughout + 6.2), stale-comment fix (10.1), verification spike (Phase 0). Out-of-scope items (ort-primary, GPU accel, z precision, fusion, web, 8-hr soak, settings UI) are not tasked, as intended.

**Placeholder scan:** Two intentional `todo!()`/"fill in" markers appear inside *test fixtures* (the `pose()` open-hand landmark array in 5.1) with an explicit instruction to replace them with a concrete `const` before shipping — flagged, not silent. Model-derived numeric constants (anchor params, tensor shapes, ROI factors) are explicitly sourced from the Phase-0 spike manifest and named reference repos, which is a real data dependency, not a hand-wave.

**Type consistency:** `HandInference::run(&Tensor) -> Vec<Tensor>` is used consistently (6.1, 6.2, 8.1). `FrameSource::next_frame(&mut Frame) -> Result<bool, _>` consistent (7, 8.1). `image_norm_to_leap_mm(Vec3, bool)` consistent (2.1, 8.1). `Hand`/`HandTrackingFrame`/`ProviderStatus` match the real codebase signatures read during design.

**Sequencing risk:** Phase 0 must complete before Phases 3/6 (it supplies tensor shapes + goldens + the runtime decision). Phases 2 and 5 (pure glue) and Phase 1 (scaffolding) have no such dependency and can proceed immediately in parallel if desired.
