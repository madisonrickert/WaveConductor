# Radiance Plan B: Body Tracking — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `BodyTrackingPlugin` in `wc-core` that runs MediaPipe BlazePose (person detector → ROI → 33 landmarks + world landmarks + 256×256 segmentation mask) on a webcam worker thread and publishes `BodyTrackingState`, `MaskTexture`, and `SilhouetteEdges` for the Radiance sketch (Plan C), activated by inserting `BodyTrackingRequest`.

**Architecture:** A parallel seam beside hand tracking, copying the proven mediapipe worker shape: a `!Send` camera built on the worker thread via a `SourceFactory`, ONNX inference via the shared `ort` backend (CoreML on macOS), results crossing to the main thread on a lock-free `rtrb` ring with newest-frame-wins dropping, and live tuning via atomics. The 256 KB mask travels through a two-ring `Box` pool (worker→main filled, main→worker recycled) so steady state allocates nothing; edge extraction and mask EMA run on the worker; One-Euro landmark smoothing runs at poll rate on the main thread. Two promotions precede the new module: `providers/mediapipe/capture/` → `input/capture/` and `inference.rs`/`inference_ort.rs` → `input/onnx/`, so both modalities share one capture and one inference stack.

**Tech Stack:** ort + CoreML EP (DirectML on Windows, CPU elsewhere), image, rtrb, bytemuck (already in the workspace graph), the existing AVFoundation/nokhwa capture backends. NO new dependencies.

## Global Constraints

- `cargo fmt --all -- --check` must pass (rustfmt.toml nightly warnings are expected and harmless).
- `cargo clippy --all-targets --all-features --workspace -- -D warnings` — lints are hard errors.
- `cargo nextest run --workspace --all-features` plus `cargo test --doc --workspace` (nextest skips doctests).
- `cargo doc --no-deps --workspace --document-private-items` with `RUSTDOCFLAGS="-D warnings"` must be clean.
- `cargo deny check` and `cargo xtask check-secrets` must pass (no home-dir paths, no emails, no secret prefixes).
- No allocation in hot paths: the worker loop, the pipeline per-frame path, and every per-frame Bevy system pre-allocate and refill with `clear()`; `Box` payloads recycle through the pool.
- No `unwrap()`/`expect()` outside `#[cfg(test)]` code.
- No `as` numeric casts where `From`/`TryFrom`/`u32::try_from` works; the two sanctioned float→int helpers carry `#[allow]` with reasons, copied from the hand pipeline.
- `///` rustdoc on every public item; `//!` module docs on every module; data flow documented at `BodyTrackingPlugin::build`.
- Doc gate builds DEFAULT features only: no intra-doc links from default-features code into `body-tracking-*`/`hand-tracking-*` gated items — use plain code spans there.
- Commit messages contain NO backticks, are passed with plain `git commit -m`, and end with the line: Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
- Deferred-build caveat: the machine is critically low on disk at planning time; all `cargo check`/`cargo nextest` steps run at execution time (free disk first per the memory note), never during planning.
- Tasks 13 and 15 are **operator-assisted** (model download needs a browser-adjacent network step and the smoke test needs a live camera); everything else is headless.

---

## Cross-cutting design decisions (read before Task 1)

These resolve the points the spec left to the plan; every task below conforms to them.

1. **Presence mechanism — direct `InteractionTimer` hook.** `poll_body_worker` takes `Option<ResMut<InteractionTimer>>` and calls `timer.mark(time.elapsed())` when it drains a person-bearing `BodyWorkerMsg::Frame`. This is the least invasive mechanism consistent with `lifecycle/idle.rs`: `reset_on_interaction`'s hand path also just ends in `InteractionTimer::mark` (hand-*bearing* frames only, empty frames ignored), and a direct mark reproduces exactly those semantics without adding a message type, without touching `lifecycle/idle.rs`, and without fabricating `HandTrackingFrame`s (which would spawn phantom hand entities through `fuse_hand_frames`). Empty/absent frames never mark. The `Option` keeps headless harnesses without the lifecycle plugin working.
2. **Mask UV space = camera-content-normalized frame space.** The landmark model emits its mask in ROI-crop space; the worker inverse-warps it into a 256×256 grid over the camera *content* rect (square-padding bars stripped, same `ContentRect` construction as the hand pipeline), and landmark `pos.xy` is published in that same space. So `MaskTexture` texel `(u, v)` and `BodyTrackingState.landmarks[i].pos.xy` coincide — the pinned "mask UV space". Mirroring is a presentation concern owned by Plan C (the sketch mirrors); this module publishes raw camera orientation.
3. **Models load on the worker thread.** Unlike the hand provider (which builds ort sessions in `start()` on the main thread at app startup), body tracking starts `OnEnter(Radiance)`; a first-launch CoreML compile would hitch the render thread. The `PipelineFactory` (models + sessions) therefore runs inside the worker thread; the backend label crosses back as `BodyWorkerMsg::Backend`.
4. **Pose detector anchor/decode constants** (verified against MediaPipe `pose_detection_cpu.pbtxt` and the OpenCV-zoo `mp_persondet.py`, 2026-07-12): input 224×224; SSD anchors with `num_layers: 5`, `strides: [8, 16, 32, 32, 32]`, `min_scale: 0.1484375`, `max_scale: 0.75`, `anchor_offset: 0.5`, `aspect_ratios: [1.0]`, `fixed_anchor_size: true` → **2254 anchors** (28²·2 + 14²·2 + 7²·6), matching the model's `[1, 2254, 12]` boxes + `[1, 2254, 1]` scores; decode scales `x = y = w = h = 224.0`, score clip ±100 then sigmoid, `min_score_thresh: 0.5`; 12 coords = 4 box + 4 keypoints×2. Keypoints: 0 = mid-hip (ROI centre), 1 = full-body circumscribing-circle point (ROI scale + rotation), 2 = mid-shoulder, 3 = upper-body point (2 and 3 unused here).
5. **ROI construction** (MediaPipe `AlignmentPointsRectsCalculator` + `RectTransformationCalculator`): centre = keypoint 0; square side = 2 × distance(kp0, kp1) × **1.25**; rotation brings the kp0→kp1 vector to vertical (target 90°). The landmark model's aux rows 33 (centre) and 34 (scale point) feed the identical formula for next-frame tracking (detect-then-track: the detector re-runs only when the track is lost).
6. **Landmark tensor layout:** `[1, 195]` = 39 rows × (x, y, z, visibility, presence); x/y/z in 256-crop pixels; visibility/presence are raw logits → sigmoid in Rust. Rows 0–32 are the published landmarks; rows 33–34 are the aux tracking alignment points; rows 35–38 unused. World `[1, 117]` = 39 × (x, y, z) metric metres, hip-centred; first 33 published. The pose-presence scalar `[1, 1]` is consumed raw as a probability (the OpenCV-zoo demo thresholds it raw at 0.5; Task 14 pins this against the vendored model exactly the way `ort_landmark_presence_is_a_probability_from_the_graph` pins the hand model). Outputs are selected **by shape**, not declared order, so the heatmap output (`[1, 64, 64, 39]` or similar) is ignored regardless of position.
7. **Idle throttle = detector-only probe.** While `BodyTrackingRequest.idle_throttle` is true the worker caps at the shared `IDLE_INFERENCE_HZ` (4 Hz, hardware capture throttle included via `FrameSource::set_capture_throttle`) and runs the detector stage only: presence flag + confidence still emit (the wake path), landmark/mask stages are skipped, the mask EMA decays so no stale silhouette lingers, and the carried track is cleared (it is stale after idle).
8. **Trait promotion naming:** the shared inference trait becomes `ModelInference` in `input/onnx/`; a `pub use ModelInference as HandInference;` compat alias keeps every hand-provider file compiling with zero edits beyond `providers/mediapipe/mod.rs`'s module declarations.

## File Structure

| File | Change | Responsibility |
| --- | --- | --- |
| `crates/wc-core/Cargo.toml` | modify | `body-tracking-mediapipe` / `body-tracking-camera` features, optional `bytemuck`, probe example entry |
| `crates/waveconductor/Cargo.toml` | modify | forward `body-tracking-mediapipe` → `wc-core/body-tracking-camera`; add to default |
| `crates/wc-core/src/lib.rs` | modify | register `BodyTrackingPlugin` in `CorePlugin` (feature-gated) |
| `crates/wc-core/src/input/mod.rs` | modify | declare `capture`, `onnx`, `body` modules |
| `crates/wc-core/src/input/capture/{mod,avfoundation,nokhwa}.rs` | move | shared webcam capture (from `providers/mediapipe/capture/`); owns `IDLE_INFERENCE_HZ` |
| `crates/wc-core/src/input/onnx/mod.rs` | move | shared `Tensor`/`InferenceError`/`ModelInference` (from `providers/mediapipe/inference.rs`) |
| `crates/wc-core/src/input/onnx/ort.rs` | move | shared `OrtInference` ort backend + CoreML cache (from `inference_ort.rs`) |
| `crates/wc-core/src/input/providers/mediapipe/mod.rs` | modify | replace `mod capture/inference/inference_ort` with `use` aliases |
| `crates/wc-core/src/input/providers/mediapipe/worker.rs` | modify | re-export `IDLE_INFERENCE_HZ` from `input::capture` |
| `crates/wc-core/src/input/body/mod.rs` | create | pinned public types, constants, diagnostics, `BodyTrackingPlugin` |
| `crates/wc-core/src/input/body/detector.rs` | create | pose SSD anchors (2254), regression decode, single-person selection |
| `crates/wc-core/src/input/body/roi.rs` | create | alignment-point ROI, 39-row landmark projection, `ContentRect` |
| `crates/wc-core/src/input/body/mask.rs` | create | crop→frame mask warp, temporal EMA, u8 quantization |
| `crates/wc-core/src/input/body/edges.rs` | create | silhouette edge extraction (≤ `MAX_EDGE_POINTS`, fixed capacity) |
| `crates/wc-core/src/input/body/transport.rs` | create | `BodyWorkerMsg`, `BodyFrame`, recycled `BodyFramePayload` pool |
| `crates/wc-core/src/input/body/smoothing.rs` | create | One-Euro landmark/world smoothing + velocity derivation |
| `crates/wc-core/src/input/body/pipeline.rs` | create | two-stage BlazePose pipeline, detect-then-track, live tuning, diagnostics |
| `crates/wc-core/src/input/body/worker.rs` | create | worker thread, rate caps, model loading, payload pool client |
| `crates/wc-core/src/input/body/systems.rs` | create | request sync, ring drain, state/mask/edges publish, presence hook |
| `crates/wc-core/examples/body_tracking_probe.rs` | create | operator live-camera probe (no Bevy app needed) |
| `assets/models/pose/{pose_detection,pose_landmark_full}.onnx` | create (operator) | vendored BlazePose models |
| `assets/models/pose/ATTRIBUTION.md`, `assets/models/pose/LICENSE` | create (operator) | provenance + Apache-2.0 |

---

### Task 1: Promote the capture module to `input/capture/`

Pure move + import updates. The capture backends currently sit inside the mediapipe provider and `avfoundation.rs` reaches into `providers/mediapipe/worker.rs` for `IDLE_INFERENCE_HZ`; that constant moves into the capture module (it is a *capture-rate* contract shared by both modalities) and the hand worker re-exports it so its tests and docs stay intact. The body feature names are declared in this task so the new `cfg(any(...))` gates never reference an undeclared feature (cargo's `unexpected_cfgs` check is a hard clippy error).

**Files:**
- Modify: `crates/wc-core/Cargo.toml`
- Move: `crates/wc-core/src/input/providers/mediapipe/capture/` → `crates/wc-core/src/input/capture/`
- Modify: `crates/wc-core/src/input/mod.rs`, `crates/wc-core/src/input/capture/mod.rs`, `crates/wc-core/src/input/capture/avfoundation.rs`, `crates/wc-core/src/input/providers/mediapipe/mod.rs`, `crates/wc-core/src/input/providers/mediapipe/worker.rs`

**Interfaces:**
- Consumes: existing `capture::{Frame, FrameSource, CaptureError, MockFrameSource, AvfFrameSource, NokhwaFrameSource}`.
- Produces: `crate::input::capture` (same public items) + `crate::input::capture::IDLE_INFERENCE_HZ`; features `body-tracking-mediapipe`, `body-tracking-camera` declared (empty consumers for now).

- [ ] **Step 1: Declare the body features and optional bytemuck dep**

In `crates/wc-core/Cargo.toml`, append to `[features]` after the `hand-tracking-mediapipe-camera` block:

```toml
# In-process MediaPipe BlazePose body tracking (the Radiance sketch's tracker).
# Shares the ort/image inference stack and the promoted input/capture +
# input/onnx modules with the hand provider, but is feature-independent of it:
# either modality builds alone. bytemuck provides the Pod derive for the
# EdgePoint storage-buffer layout (already in the workspace graph via
# wc-sketches). CI-testable headless against vendored models.
body-tracking-mediapipe = ["dep:ort", "dep:image", "dep:bytemuck"]
# Adds the production webcam FrameSource for body tracking — the same shared
# capture backends (AVFoundation on macOS, nokhwa elsewhere) the hand
# provider's -camera feature enables.
body-tracking-camera = [
    "body-tracking-mediapipe",
    "dep:nokhwa",
    "dep:objc2",
    "dep:objc2-foundation",
    "dep:objc2-av-foundation",
    "dep:objc2-core-video",
    "dep:objc2-core-media",
    "dep:dispatch2",
]
```

In `[dependencies]` (the unconditional table, next to `blake3`), add:

```toml
bytemuck = { workspace = true, optional = true }
```

- [ ] **Step 2: Move the directory**

```bash
git mv crates/wc-core/src/input/providers/mediapipe/capture crates/wc-core/src/input/capture
```

- [ ] **Step 3: Declare the module in `input/mod.rs`**

In `crates/wc-core/src/input/mod.rs`, after `pub mod button;`:

```rust
/// Shared webcam frame capture: the `FrameSource` trait, the platform
/// backends (AVFoundation on macOS, nokhwa elsewhere), and the test
/// `MockFrameSource`. Consumed by the MediaPipe hand provider and by the
/// body-tracking worker, so it lives beside — not inside — either.
#[cfg(any(feature = "hand-tracking-mediapipe", feature = "body-tracking-mediapipe"))]
pub mod capture;
```

- [ ] **Step 4: Update `capture/mod.rs` gates and adopt the idle-rate constant**

In `crates/wc-core/src/input/capture/mod.rs`:

(a) Update the module doc's feature reference: replace the sentence fragment "Both backends are gated on the `hand-tracking-mediapipe-camera` feature." with "Both backends are gated on the camera features (`hand-tracking-mediapipe-camera` or `body-tracking-camera`)."

(b) Replace all four backend `cfg` gates at the bottom of the file:

```rust
/// Production webcam backend, selected per platform.
#[cfg(all(
    any(feature = "hand-tracking-mediapipe-camera", feature = "body-tracking-camera"),
    not(target_os = "macos")
))]
mod nokhwa;

#[cfg(all(
    any(feature = "hand-tracking-mediapipe-camera", feature = "body-tracking-camera"),
    target_os = "macos"
))]
mod avfoundation;
#[cfg(all(
    any(feature = "hand-tracking-mediapipe-camera", feature = "body-tracking-camera"),
    target_os = "macos"
))]
pub use avfoundation::AvfFrameSource;
#[cfg(all(
    any(feature = "hand-tracking-mediapipe-camera", feature = "body-tracking-camera"),
    not(target_os = "macos")
))]
pub use nokhwa::NokhwaFrameSource;
```

(c) Add the idle-rate constant (moved verbatim, doc included, from `providers/mediapipe/worker.rs` — delete it there in Step 6). Place it directly after the `CaptureError` enum:

```rust
/// Inference/capture cap (Hz) applied while a tracking worker's idle throttle
/// is set, i.e. the sketch is in `Idle`/`Screensaver` with no audience present.
/// Shared by every camera-consuming modality (MediaPipe hands, BlazePose body)
/// so their hardware capture throttles and wake-latency budgets agree.
///
/// Wake-latency contract: 4 Hz means a worst-case wake of one throttle period
/// (250 ms) plus one full pipeline run (tens of ms) ≈ **300 ms** before the
/// first hand-bearing (or person-bearing) frame resets the idle timer and
/// flips the app back to `Active`/full rate. Against the 30 s idle threshold
/// that entry latency is imperceptible, while the sustained load drop is the
/// bulk of the multi-hour idle thermal win.
///
/// On backends that honor [`FrameSource::set_capture_throttle`] (macOS
/// `AVFoundation`), the *camera* drops to this same rate while idle, so the
/// freshest frame is at most one period (250 ms) old when processed. No added
/// wake latency; the sensor/ISP simply do less work.
pub const IDLE_INFERENCE_HZ: u32 = 4;
```

- [ ] **Step 5: Repoint `avfoundation.rs`**

In `crates/wc-core/src/input/capture/avfoundation.rs`, replace the single line:

```rust
use super::super::worker::IDLE_INFERENCE_HZ;
```

with:

```rust
use super::IDLE_INFERENCE_HZ;
```

- [ ] **Step 6: Repoint the hand provider**

In `crates/wc-core/src/input/providers/mediapipe/worker.rs`, delete the `pub const IDLE_INFERENCE_HZ: u32 = 4;` constant *and its full doc comment* (moved in Step 4c), replacing them with:

```rust
/// Idle inference cap, shared with the capture layer (and the body-tracking
/// worker) — see the constant's wake-latency contract in `input::capture`.
/// Re-exported here so this worker's tests and docs keep their historical
/// `worker::IDLE_INFERENCE_HZ` path.
pub use crate::input::capture::IDLE_INFERENCE_HZ;
```

In `crates/wc-core/src/input/providers/mediapipe/mod.rs`, replace the module declaration line `mod capture;` with:

```rust
// The capture module was promoted to `crate::input::capture` (shared with the
// body-tracking worker). This alias keeps every `self::capture::…` /
// `super::capture::…` path in this provider compiling unchanged.
use crate::input::capture;
```

- [ ] **Step 7: Verify (deferred build)**

Run: `cargo check -p wc-core --features hand-tracking-mediapipe-camera`
Expected: clean. Then run the moved module's tests plus the worker/provider tests that exercise it:

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe input::providers::mediapipe input::capture`
Expected: PASS (all pre-existing capture, worker, and provider tests green, including `idle_metric_label_is_hz_independent`, which pins the re-exported constant at 4).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(input): promote webcam capture to shared input/capture module" -m "Pure move plus import updates: the FrameSource trait, platform backends, and MockFrameSource now live beside the providers so hand and body tracking share one capture stack. IDLE_INFERENCE_HZ moves to the capture layer (re-exported by the hand worker). Declares the body-tracking-mediapipe / body-tracking-camera features consumed by later tasks." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: Promote ONNX inference to `input/onnx/`

The `Tensor`/`InferenceError`/trait module and the `OrtInference` backend (with its CoreML per-model cache keys — logic that must NOT be duplicated) move out of the hand provider. The trait is renamed `ModelInference`; a re-export alias keeps hand files untouched.

**Files:**
- Move: `crates/wc-core/src/input/providers/mediapipe/inference.rs` → `crates/wc-core/src/input/onnx/mod.rs`
- Move: `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs` → `crates/wc-core/src/input/onnx/ort.rs`
- Modify: `crates/wc-core/src/input/mod.rs`, `crates/wc-core/src/input/onnx/mod.rs`, `crates/wc-core/src/input/onnx/ort.rs`, `crates/wc-core/src/input/providers/mediapipe/mod.rs`

**Interfaces:**
- Consumes: existing `Tensor`, `InferenceError`, `OrtInference`.
- Produces: `crate::input::onnx::{ModelInference, HandInference (alias), InferenceError, Tensor}`, `crate::input::onnx::ort::{OrtInference, BACKEND_CPU, BACKEND_COREML, BACKEND_DIRECTML}` — everything Tasks 9/11 build on.

- [ ] **Step 1: Move the files**

```bash
mkdir -p crates/wc-core/src/input/onnx
git mv crates/wc-core/src/input/providers/mediapipe/inference.rs crates/wc-core/src/input/onnx/mod.rs
git mv crates/wc-core/src/input/providers/mediapipe/inference_ort.rs crates/wc-core/src/input/onnx/ort.rs
```

- [ ] **Step 2: Edit `onnx/mod.rs`**

(a) Update the module doc's first paragraph to read (keep the rest):

```rust
//! ONNX inference behind a runtime-agnostic trait, shared by the MediaPipe
//! hand pipeline and the BlazePose body pipeline.
//!
//! Defines the shared types used by all inference backends: [`Tensor`] (a dense
//! row-major `f32` buffer with a shape), [`InferenceError`], and
//! [`ModelInference`] (run one ONNX model stage, input tensor → raw output
//! tensors). The concrete implementation is [`ort::OrtInference`]
//! (`ort`/ONNX Runtime with `CoreML` acceleration on macOS).
```

(b) Add the submodule declaration after the `use thiserror::Error;` import:

```rust
/// ONNX Runtime (`ort`) backend; the sole concrete [`ModelInference`]
/// implementation used by the hand and body pipelines.
pub mod ort;
```

(c) Rename the trait: `pub trait HandInference: Send {` → `pub trait ModelInference: Send {` (doc comment unchanged), and add directly below the trait:

```rust
/// Historical name from when this trait lived inside the hand provider; the
/// mediapipe hand modules still import it as `HandInference`. New code uses
/// [`ModelInference`].
pub use ModelInference as HandInference;
```

- [ ] **Step 3: Edit `onnx/ort.rs`**

(a) Replace the import `use super::inference::{HandInference, InferenceError, Tensor};` with `use super::{InferenceError, ModelInference, Tensor};`.
(b) Replace `impl HandInference for OrtInference {` with `impl ModelInference for OrtInference {`.
(c) Change the three backend-label constants from `pub(super)` to `pub(crate)` (the hand provider now reaches them through the alias path).
(d) In the module doc, change "for the MediaPipe hand-tracking pipeline" to "for the MediaPipe hand and BlazePose body pipelines", and update the `HandInference` mention to `ModelInference`.
(e) The vendored-model tests keep reading `../../assets/models/hand` via `CARGO_MANIFEST_DIR` — unchanged (the manifest dir did not move).

- [ ] **Step 4: Declare the module and alias the old paths**

In `crates/wc-core/src/input/mod.rs`, after the `capture` declaration from Task 1:

```rust
/// Shared ONNX inference (tensor types + the ort backend with its CoreML
/// per-model cache), consumed by the MediaPipe hand provider and the
/// body-tracking pipeline.
#[cfg(any(feature = "hand-tracking-mediapipe", feature = "body-tracking-mediapipe"))]
pub mod onnx;
```

In `crates/wc-core/src/input/providers/mediapipe/mod.rs`, delete the two declarations `mod inference;` and (with its doc comment) `mod inference_ort;`, adding in their place:

```rust
// The inference module was promoted to `crate::input::onnx` (shared with the
// body pipeline). These aliases keep every `super::inference::…` /
// `super::inference_ort::…` path in this provider compiling unchanged.
use crate::input::onnx as inference;
use crate::input::onnx::ort as inference_ort;
```

- [ ] **Step 5: Verify (deferred build)**

Run: `cargo check -p wc-core --features hand-tracking-mediapipe-camera`
Expected: clean (all hand modules resolve `HandInference` through the alias; the ort tests still see the vendored hand models).

Run: `cargo nextest run -p wc-core --features hand-tracking-mediapipe input::onnx input::providers::mediapipe`
Expected: PASS — in particular `backend_label_is_one_of_the_known_values`, `ort_palm_model_runs_and_emits_raw_box_and_score_tensors`, and the full pipeline/worker suites.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(input): promote ort inference to shared input/onnx module" -m "Tensor, InferenceError, and the ort backend (CoreML per-model cache included) move out of the hand provider so the BlazePose body pipeline reuses them instead of duplicating cache-key logic. Trait renamed ModelInference with a HandInference re-export alias, so hand-provider files compile unchanged." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Body module skeleton — pinned public types, plugin shell, app wiring

Creates `input/body/mod.rs` with **exactly** the pinned cross-plan types (plus the diagnostics surface the spec requires), registers the plugin shell in `CorePlugin`, and forwards the binary feature. Submodules are declared by the tasks that create them.

**Files:**
- Create: `crates/wc-core/src/input/body/mod.rs`
- Modify: `crates/wc-core/src/input/mod.rs`, `crates/wc-core/src/lib.rs`, `crates/waveconductor/Cargo.toml`

**Interfaces:**
- Consumes: `bevy::prelude::*`, `bytemuck::{Pod, Zeroable}`, `crate::platform::assets::asset_root`.
- Produces (pinned, later tasks and Plan C rely on these verbatim): `BODY_LANDMARK_COUNT`, `MAX_EDGE_POINTS`, `MASK_SIZE`, `BodyTrackingRequest`, `BodyLandmark`, `BodyTrackingState`, `MaskTexture`, `EdgePoint`, `SilhouetteEdges`. Plus (additive): `MASK_SIZE_U32`, `BodyTrackingStatus`, `BodyTrackingDiagnostics`, `BodyTrackingConfig`, `landmark_index`, `BodyTrackingPlugin`.

- [ ] **Step 1: Write the failing test**

The tests live at the footer of the new `mod.rs` (they fail to compile until Step 3's types exist; that IS the failing state for a wiring task). Test module content:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_defaults_are_neutral() {
        let s = BodyTrackingState::default();
        assert!(!s.present);
        assert_eq!(s.confidence, 0.0);
        assert_eq!(s.landmarks[0].visibility, 0.0);
        assert_eq!(s.world_landmarks[32], Vec3::ZERO);
        assert_eq!(s.velocities[15], Vec3::ZERO);
        assert_eq!(s.timestamp, Duration::ZERO);
    }

    #[test]
    fn edge_point_is_pod_with_gpu_layout() {
        // Plan C uploads SilhouetteEdges as a storage buffer via bytemuck; the
        // layout must be two tightly-packed vec2<f32>s (16 bytes).
        assert_eq!(std::mem::size_of::<EdgePoint>(), 16);
        assert_eq!(std::mem::offset_of!(EdgePoint, pos), 0);
        assert_eq!(std::mem::offset_of!(EdgePoint, normal), 8);
        let p = EdgePoint {
            pos: Vec2::new(0.25, 0.5),
            normal: Vec2::new(0.0, -1.0),
        };
        assert_eq!(bytemuck::bytes_of(&p).len(), 16);
    }

    #[test]
    fn silhouette_edges_preallocates_full_capacity() {
        let e = SilhouetteEdges::default();
        assert!(e.points.is_empty());
        assert_eq!(e.points.capacity(), MAX_EDGE_POINTS);
        assert_eq!(e.generation, 0);
    }

    #[test]
    fn mask_size_constants_agree() {
        assert_eq!(usize::try_from(MASK_SIZE_U32), Ok(MASK_SIZE));
    }

    #[test]
    fn diagnostics_default_shows_not_started_backend() {
        let d = BodyTrackingDiagnostics::default();
        assert_eq!(d.status, BodyTrackingStatus::Inactive);
        assert_eq!(d.backend, "not started");
        assert!(d.last_error.is_none());
    }

    #[test]
    fn config_defaults_mirror_the_hand_provider() {
        let c = BodyTrackingConfig::default();
        assert_eq!(c.camera_index, 0);
        assert_eq!(c.max_inference_hz, 30);
        assert!(c.model_dir.ends_with("models/pose"));
    }

    #[test]
    fn plugin_shell_initializes_resources() {
        let mut app = App::new();
        app.add_plugins(BodyTrackingPlugin);
        assert!(app.world().contains_resource::<BodyTrackingState>());
        assert!(app.world().contains_resource::<BodyTrackingDiagnostics>());
        assert!(app.world().contains_resource::<SilhouetteEdges>());
        assert!(app.world().contains_resource::<BodyTrackingConfig>());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body`
Expected: FAIL — compile error (`input::body` module does not exist yet / types unresolved).

- [ ] **Step 3: Write minimal implementation**

Create `crates/wc-core/src/input/body/mod.rs`:

```rust
//! Webcam body tracking: MediaPipe BlazePose person detection → ROI → 33
//! landmarks, metric world landmarks, and a 256×256 person segmentation mask.
//!
//! A parallel seam beside hand tracking — not a
//! [`crate::input::provider::HandTrackingProvider`] implementation (that trait
//! bakes in 21-landmark hands). One worker thread copies the proven mediapipe
//! worker shape; results publish as plain resources the Radiance sketch
//! consumes. See [`BodyTrackingPlugin`] for the full data flow.
//!
//! Activation contract: some sketch (Radiance) INSERTS [`BodyTrackingRequest`]
//! to start the camera + worker and REMOVES it to stop them. While a request
//! exists, a person-bearing frame resets the idle
//! `InteractionTimer` with the same semantics as hand-bearing frames in
//! `reset_on_interaction` (empty frames are ignored).

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::*;
use bytemuck::{Pod, Zeroable};

/// Number of BlazePose body landmarks published to consumers.
pub const BODY_LANDMARK_COUNT: usize = 33;

/// Fixed capacity of the silhouette edge list ([`SilhouetteEdges`]).
pub const MAX_EDGE_POINTS: usize = 2048;

/// Side length of the person segmentation mask (256×256, `R8Unorm`).
pub const MASK_SIZE: usize = 256;

/// [`MASK_SIZE`] as `u32` for texture extents (pinned equal by a test, so no
/// runtime conversion is ever needed).
pub const MASK_SIZE_U32: u32 = 256;

/// MediaPipe pose landmark indices for the subset Plan C uses as limb-impulse
/// sources. The full 33-point topology is the standard BlazePose layout.
pub mod landmark_index {
    /// Head reference point.
    pub const NOSE: usize = 0;
    /// Left wrist.
    pub const LEFT_WRIST: usize = 15;
    /// Right wrist.
    pub const RIGHT_WRIST: usize = 16;
    /// Left hip.
    pub const LEFT_HIP: usize = 23;
    /// Right hip.
    pub const RIGHT_HIP: usize = 24;
    /// Left ankle.
    pub const LEFT_ANKLE: usize = 27;
    /// Right ankle.
    pub const RIGHT_ANKLE: usize = 28;
}

/// Activation contract: INSERT this resource to start the worker + camera;
/// REMOVE it to stop. Sketch-agnostic — Plan C inserts it
/// `OnEnter(Radiance)` and removes it `OnExit`.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BodyTrackingRequest {
    /// `true` during `Idle`/`Screensaver`: the worker drops to a detector-only
    /// presence probe at the shared idle rate (4 Hz class, hardware capture
    /// throttle included) so a person walking up still re-activates the
    /// sketch. Driven by Plan C from `SketchActivity`.
    pub idle_throttle: bool,
}

/// One tracked body landmark in mask-UV space.
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyLandmark {
    /// `x`,`y` screen-normalized `0..1` in mask UV space (the camera content
    /// rect — the same space [`MaskTexture`] texels live in); `z` is the
    /// model's relative depth (ROI-scaled, not metric).
    pub pos: Vec3,
    /// Per-landmark visibility probability in `0..1`.
    pub visibility: f32,
}

/// Continuous body-tracking snapshot. Always present once
/// [`BodyTrackingPlugin`] is added; `present == false` when there is no
/// request or no person. Landmarks and world landmarks are One-Euro smoothed
/// at poll rate; velocities are the smoothed screen-space derivatives.
#[derive(Resource, Clone, Debug)]
pub struct BodyTrackingState {
    /// Whether a person is currently tracked.
    pub present: bool,
    /// Track confidence (the landmark model's pose-presence probability, or
    /// the detector score while in the idle detector-only probe).
    pub confidence: f32,
    /// Screen-normalized landmarks + visibility (mask UV space).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (metres, hip-centred), One-Euro smoothed.
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
    /// Smoothed landmark velocities, screen-normalized units/sec.
    pub velocities: [Vec3; BODY_LANDMARK_COUNT],
    /// Worker-relative capture timestamp of the underlying inference frame.
    pub timestamp: Duration,
}

impl Default for BodyTrackingState {
    fn default() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            velocities: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            timestamp: Duration::ZERO,
        }
    }
}

/// Handle to the reused 256×256 `R8Unorm` person-mask image (EMA-smoothed).
/// Mask bytes are written in place each body frame; Bevy re-uploads on
/// mutation. Inserted at startup when `Assets<Image>` exists (i.e. in any app
/// with the asset plugin; absent in bare headless harnesses).
#[derive(Resource, Clone)]
pub struct MaskTexture(pub Handle<Image>);

/// One silhouette edge sample: position + outward normal, both in mask UV
/// space. `#[repr(C)]` + `Pod` so Plan C can upload the whole list as a
/// storage buffer with `bytemuck`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct EdgePoint {
    /// Position in mask UV space `0..1`.
    pub pos: Vec2,
    /// Outward unit normal (points from inside the person toward outside).
    pub normal: Vec2,
}

/// CPU edge list extracted on the worker where the EMA-smoothed mask crosses
/// 0.5. Refilled in place (`clear()`, never realloc — capacity is
/// [`MAX_EDGE_POINTS`] by construction).
#[derive(Resource)]
pub struct SilhouetteEdges {
    /// Edge samples for the latest body frame.
    pub points: Vec<EdgePoint>,
    /// Bumped on each new body frame so consumers can skip re-upload.
    pub generation: u64,
}

impl Default for SilhouetteEdges {
    fn default() -> Self {
        Self {
            points: Vec::with_capacity(MAX_EDGE_POINTS),
            generation: 0,
        }
    }
}

/// Coarse body-tracking lifecycle state, surfaced in diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyTrackingStatus {
    /// No request; nothing running.
    #[default]
    Inactive,
    /// Worker spawned; models/camera still coming up.
    Starting,
    /// Camera frames flowing through the pipeline.
    Streaming,
    /// The camera could not be opened or a read failed.
    CameraUnavailable,
    /// Model load/session build failed (see `last_error`).
    Failed,
}

impl BodyTrackingStatus {
    /// Static label for panels/logs (no per-frame allocation).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Starting => "starting",
            Self::Streaming => "streaming",
            Self::CameraUnavailable => "camera unavailable",
            Self::Failed => "failed",
        }
    }
}

/// Body-tracking diagnostics: backend label (a silent CPU fallback must be
/// visible, matching hand-tracking practice), status, and worker counters.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BodyTrackingDiagnostics {
    /// Lifecycle status.
    pub status: BodyTrackingStatus,
    /// Inference backend label (`"ort/CoreML"`, `"ort/CPU"`, mixed states) as
    /// reported by the worker after building its sessions.
    pub backend: &'static str,
    /// Negotiated camera format label, when the source reports one.
    pub camera_format: Option<String>,
    /// Most recent worker/pipeline error string.
    pub last_error: Option<String>,
    /// Wall time between the last two processed frames (effective inference
    /// period).
    pub inference_interval: Duration,
    /// Cumulative camera frames dropped by the rate cap / idle throttle.
    pub dropped_frames: u64,
    /// Cumulative ring-buffer backpressure drops (slow consumer, not camera).
    pub ring_full_drops: u64,
    /// Cumulative pipeline (inference) errors.
    pub pipeline_errors: u64,
    /// Whether the idle detector-only throttle is currently requested.
    pub idle_throttled: bool,
}

impl Default for BodyTrackingDiagnostics {
    fn default() -> Self {
        Self {
            status: BodyTrackingStatus::Inactive,
            backend: "not started",
            camera_format: None,
            last_error: None,
            inference_interval: Duration::ZERO,
            dropped_frames: 0,
            ring_full_drops: 0,
            pipeline_errors: 0,
            idle_throttled: false,
        }
    }
}

/// Construction-time configuration (camera index, rate cap, model directory).
/// Inserted with defaults by the plugin; override before the first
/// [`BodyTrackingRequest`] to change it.
#[derive(Resource, Clone, Debug)]
pub struct BodyTrackingConfig {
    /// Camera index to open (0 = default device).
    pub camera_index: u32,
    /// Full-rate inference cap in Hz (0 = uncapped). 30 matches the hand
    /// provider: body tracking does not need full frame rate, and capping
    /// leaves CPU/thermal headroom.
    pub max_inference_hz: u32,
    /// Directory holding `pose_detection.onnx` and `pose_landmark_full.onnx`.
    /// Resolved at runtime via `platform::assets::asset_root` so the path is
    /// correct in dev, release, and macOS `.app` bundle deployments.
    pub model_dir: PathBuf,
}

impl Default for BodyTrackingConfig {
    fn default() -> Self {
        Self {
            camera_index: 0,
            max_inference_hz: 30,
            model_dir: crate::platform::assets::asset_root().join("models/pose"),
        }
    }
}

/// Wires body tracking into the Bevy [`App`]. Resource shell in this task;
/// Task 12 adds the startup + `PreUpdate` systems and the full data-flow doc.
pub struct BodyTrackingPlugin;

impl Plugin for BodyTrackingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BodyTrackingState>()
            .init_resource::<BodyTrackingDiagnostics>()
            .init_resource::<SilhouetteEdges>()
            .init_resource::<BodyTrackingConfig>();
    }
}
```

…followed by the Step 1 test module.

Then wire the module and the app:

(a) `crates/wc-core/src/input/mod.rs`, after the `onnx` declaration:

```rust
/// Webcam body tracking (BlazePose person detector + landmark/segmentation
/// worker), consumed by the Radiance sketch.
#[cfg(feature = "body-tracking-mediapipe")]
pub mod body;
```

(b) `crates/wc-core/src/lib.rs`, in `CorePlugin::build` directly after `app.add_plugins(input::HandTrackingPlugin);`:

```rust
// Body tracking (BlazePose person detector + landmark/segmentation worker).
// Inert until a sketch inserts BodyTrackingRequest (Radiance, Plan C).
#[cfg(feature = "body-tracking-mediapipe")]
app.add_plugins(input::body::BodyTrackingPlugin);
```

(c) `crates/waveconductor/Cargo.toml`: add to `[features]`:

```toml
# In-process MediaPipe BlazePose body tracking (Radiance's tracker; pulls the
# camera backends). Inert until a sketch requests it, so shipping it in the
# default set costs nothing at runtime.
body-tracking-mediapipe = ["wc-core/body-tracking-camera"]
```

and extend the default list:

```toml
default = ["hand-tracking-gestures", "hand-tracking-mediapipe", "body-tracking-mediapipe"]
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body`
Expected: PASS (7 tests). Also run: `cargo check -p wc-core --features body-tracking-mediapipe` and `cargo check -p wc-core` (default features must be untouched).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): body-tracking module skeleton with pinned public types" -m "BodyTrackingRequest/State, MaskTexture, SilhouetteEdges, EdgePoint (Pod), diagnostics surface, config resource, and the plugin shell registered in CorePlugin behind body-tracking-mediapipe. Binary forwards the feature by default." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Pose detector post-processing (`detector.rs`)

SSD anchors, raw-regression decode, and single-person selection, adapted from the palm detector's `anchors.rs`/`palm.rs` with the pose numbers from design decision 4.

**Files:**
- Create: `crates/wc-core/src/input/body/detector.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod detector;`)

**Interfaces:**
- Consumes: `bevy::math::Vec2`.
- Produces: `Anchor`, `generate_pose_anchors()`, `POSE_ANCHOR_COUNT = 2254`, `POSE_KEYPOINTS = 4`, `POSE_REGRESSION_LEN = 12`, `DETECTOR_INPUT = 224`, `Rect`, `PersonDetection`, `sigmoid`, `decode_pose_detections_into`, `best_person` — consumed by Task 9's pipeline.

- [ ] **Step 1: Write the failing test**

Test module (file footer of the new `detector.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_2254_anchors_for_pose_224() {
        // 28×28×2 (stride 8) + 14×14×2 (stride 16) + 7×7×6 (stride-32 group
        // of three layers) = 2254, matching the model's [1, 2254, 12] output.
        let anchors = generate_pose_anchors();
        assert_eq!(anchors.len(), POSE_ANCHOR_COUNT);
    }

    #[test]
    fn anchor_grid_layout_matches_the_ssd_config() {
        let anchors = generate_pose_anchors();
        // Layer 0: stride 8 → 28×28 grid; first cell centre = 0.5/28.
        assert!((anchors[0].cx - 0.5 / 28.0).abs() < 1e-6, "cx={}", anchors[0].cx);
        assert!((anchors[0].cy - 0.5 / 28.0).abs() < 1e-6, "cy={}", anchors[0].cy);
        // The two stride-8 anchors at a location share a centre.
        assert_eq!(anchors[0], anchors[1]);
        // Stride-8 layer holds 28×28×2 = 1568; index 1568 is the first
        // stride-16 anchor (14×14 grid, centre 0.5/14).
        assert!((anchors[1568].cx - 0.5 / 14.0).abs() < 1e-6);
        assert!((anchors[1568].cy - 0.5 / 14.0).abs() < 1e-6);
        // Stride-16 layer holds 14×14×2 = 392; index 1960 is the first
        // stride-32 anchor (7×7 grid, six anchors per location, centre 0.5/7).
        assert!((anchors[1960].cx - 0.5 / 7.0).abs() < 1e-6);
        assert_eq!(anchors[1960], anchors[1965]);
        // Last anchor: bottom-right of the 7×7 grid.
        let last = anchors[anchors.len() - 1];
        assert!((last.cx - 6.5 / 7.0).abs() < 1e-6);
        assert!((last.cy - 6.5 / 7.0).abs() < 1e-6);
    }

    #[test]
    fn decode_places_box_and_keypoints_at_anchor_for_zero_offsets() {
        let anchor = Anchor { cx: 0.5, cy: 0.5 };
        let mut raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        raw[2] = DETECTOR_SCALE * 0.4; // width 0.4
        raw[3] = DETECTOR_SCALE * 0.4; // height 0.4
        let mut out = Vec::new();
        decode_pose_detections_into(&raw, &[100.0], &[anchor], 0.5, &mut out);
        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert!(d.score > 0.99); // raw 100 → sigmoid ≈ 1
        assert!((d.bbox.xmin - 0.3).abs() < 1e-5, "{:?}", d.bbox);
        assert!((d.bbox.ymax - 0.7).abs() < 1e-5, "{:?}", d.bbox);
        for kp in &d.keypoints {
            assert!((kp.x - 0.5).abs() < 1e-5 && (kp.y - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn decode_offsets_keypoints_relative_to_the_anchor() {
        let anchor = Anchor { cx: 0.25, cy: 0.75 };
        let mut raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        // Keypoint 1 (full-body scale point): +0.1 in x, −0.2 in y.
        raw[6] = DETECTOR_SCALE * 0.1;
        raw[7] = -DETECTOR_SCALE * 0.2;
        let mut out = Vec::new();
        decode_pose_detections_into(&raw, &[100.0], &[anchor], 0.5, &mut out);
        let kp1 = out[0].keypoints[1];
        assert!((kp1.x - 0.35).abs() < 1e-5, "kp1={kp1:?}");
        assert!((kp1.y - 0.55).abs() < 1e-5, "kp1={kp1:?}");
    }

    #[test]
    fn decode_drops_below_threshold_scores() {
        let anchor = Anchor { cx: 0.5, cy: 0.5 };
        let raw = vec![0.0_f32; POSE_REGRESSION_LEN];
        let mut out = Vec::new();
        // raw 0 → sigmoid 0.5; threshold 0.6 drops it.
        decode_pose_detections_into(&raw, &[0.0], &[anchor], 0.6, &mut out);
        assert!(out.is_empty());
    }

    fn det(score: f32, x: f32, y: f32, size: f32) -> PersonDetection {
        PersonDetection {
            score,
            bbox: Rect {
                xmin: x,
                ymin: y,
                xmax: x + size,
                ymax: y + size,
            },
            keypoints: [Vec2::new(x, y); POSE_KEYPOINTS],
        }
    }

    #[test]
    fn best_person_blends_the_top_cluster_and_ignores_far_detections() {
        let dets = vec![
            det(0.7, 5.0, 5.0, 1.0),   // a second person, far away — excluded
            det(0.9, 0.0, 0.0, 1.0),   // seed (argmax)
            det(0.8, 0.05, 0.05, 1.0), // overlaps the seed — blended in
        ];
        let best = best_person(&dets, 0.3).expect("a detection above zero");
        // Carries the seed's (maximal) score; the centre blends toward the
        // overlapping detection but never toward the far one.
        assert!((best.score - 0.9).abs() < 1e-6);
        assert!(best.bbox.xmin > 0.0 && best.bbox.xmin < 0.05, "{:?}", best.bbox);
        assert!(best.keypoints[0].x < 0.05);
    }

    #[test]
    fn best_person_of_empty_is_none() {
        assert!(best_person(&[], 0.3).is_none());
    }

    #[test]
    fn sigmoid_is_bounded_and_monotonic() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
        assert!(sigmoid(1.0) > sigmoid(0.5));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::detector`
Expected: FAIL — compile error (items unresolved).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod detector;` to `body/mod.rs` (alphabetical, before the type definitions), then create `crates/wc-core/src/input/body/detector.rs` above the test module:

```rust
//! BlazePose person-detector post-processing: SSD anchor generation, raw
//! regression decode, and single-person selection.
//!
//! The pose-detection ONNX graph emits raw box/keypoint regressions relative
//! to a fixed anchor grid (no anchor logic in the graph), exactly like the
//! palm detector. This module reproduces MediaPipe's `SsdAnchorsCalculator`
//! for the 224×224 pose model (from `pose_detection_cpu.pbtxt`): 5 layers,
//! strides `[8, 16, 32, 32, 32]`, one square aspect ratio plus one
//! interpolated scale (2 anchors per location per same-stride layer),
//! `fixed_anchor_size` (sizes come from the regression), offsets 0.5 →
//! `28²·2 + 14²·2 + 7²·6 = 2254` anchors, matching the `[1, 2254, 12]` boxes
//! + `[1, 2254, 1]` scores outputs. Decode scales are all 224; raw scores are
//! clipped to ±100 then sigmoided.
//!
//! Radiance tracks ONE primary dancer, so instead of full weighted NMS this
//! module selects the argmax-score detection and score-blends every detection
//! overlapping it (IoU ≥ threshold) — the first output of MediaPipe's
//! WEIGHTED NMS, which is all `SplitDetectionVectorCalculator {0..1}` keeps
//! upstream. Multi-person is deliberately out of scope (spec non-goal).

use bevy::math::Vec2;

/// Number of SSD anchors for the 224×224 pose detector (see module docs).
pub const POSE_ANCHOR_COUNT: usize = 2254;

/// Keypoints per detection: 0 = mid-hip (ROI centre), 1 = full-body
/// circumscribing-circle point (ROI scale/rotation), 2 = mid-shoulder,
/// 3 = upper-body point (2 and 3 unused by this pipeline).
pub const POSE_KEYPOINTS: usize = 4;

/// Floats per anchor in the raw box tensor: 4 box + `2·POSE_KEYPOINTS`.
pub const POSE_REGRESSION_LEN: usize = 4 + 2 * POSE_KEYPOINTS;

/// Detector model input side in pixels (224×224).
pub const DETECTOR_INPUT: u32 = 224;

/// Regression divisor (`x_scale = y_scale = w_scale = h_scale = 224.0`).
pub const DETECTOR_SCALE: f32 = 224.0;

/// Symmetric clip applied to raw scores before the sigmoid
/// (`score_clipping_thresh: 100.0`).
pub const SCORE_CLIP: f32 = 100.0;

/// One SSD anchor centre, normalized to `[0, 1]` over the model input.
/// `fixed_anchor_size` means width/height are always 1.0, so only the centre
/// is stored; real box sizes come from the regression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Normalized centre x in `[0, 1]`.
    pub cx: f32,
    /// Normalized centre y in `[0, 1]`.
    pub cy: f32,
}

/// Generate the fixed anchor grid for the 224×224 pose detector (module docs
/// have the parameter provenance). Anchor order matches the model's output
/// rows so regressions can be indexed by anchor.
#[must_use]
pub fn generate_pose_anchors() -> Vec<Anchor> {
    // pose_detection_cpu.pbtxt: strides [8, 16, 32, 32, 32]; consecutive equal
    // strides accumulate anchors at one feature-map resolution — aspect_ratios
    // [1.0] contributes 1 and the interpolated scale contributes 1, so each
    // same-stride layer adds 2 anchors per location.
    const STRIDES: [u32; 5] = [8, 16, 32, 32, 32];
    let mut anchors = Vec::with_capacity(POSE_ANCHOR_COUNT);
    let mut layer = 0;
    while layer < STRIDES.len() {
        let mut anchors_per_location = 0_usize;
        let mut last = layer;
        while last < STRIDES.len() && STRIDES[last] == STRIDES[layer] {
            anchors_per_location += 2;
            last += 1;
        }
        let stride = STRIDES[layer];
        let fm = DETECTOR_INPUT.div_ceil(stride);
        for y in 0..fm {
            // anchor_offset_x/y = 0.5: cell centres.
            let cy = (grid_f32(y) + 0.5) / grid_f32(fm);
            for x in 0..fm {
                let cx = (grid_f32(x) + 0.5) / grid_f32(fm);
                for _ in 0..anchors_per_location {
                    anchors.push(Anchor { cx, cy });
                }
            }
        }
        layer = last;
    }
    anchors
}

/// Axis-aligned rectangle in normalized `[0, 1]` image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub xmin: f32,
    /// Top edge.
    pub ymin: f32,
    /// Right edge.
    pub xmax: f32,
    /// Bottom edge.
    pub ymax: f32,
}

impl Rect {
    /// Intersection-over-union with another rectangle.
    #[must_use]
    pub fn iou(&self, other: &Rect) -> f32 {
        let ix0 = self.xmin.max(other.xmin);
        let iy0 = self.ymin.max(other.ymin);
        let ix1 = self.xmax.min(other.xmax);
        let iy1 = self.ymax.min(other.ymax);
        let inter = (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0);
        let a = (self.xmax - self.xmin).max(0.0) * (self.ymax - self.ymin).max(0.0);
        let b = (other.xmax - other.xmin).max(0.0) * (other.ymax - other.ymin).max(0.0);
        let union = a + b - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

/// A decoded person detection in normalized image coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonDetection {
    /// Sigmoid confidence in `[0, 1]`.
    pub score: f32,
    /// Bounding box.
    pub bbox: Rect,
    /// The 4 alignment keypoints (see [`POSE_KEYPOINTS`]).
    pub keypoints: [Vec2; POSE_KEYPOINTS],
}

/// Numerically-stable logistic sigmoid.
#[must_use]
pub fn sigmoid(x: f32) -> f32 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Decode raw model outputs into detections above `score_threshold`, into a
/// reused `out` buffer (`clear()` keeps capacity — the pipeline reuses one
/// buffer across frames, so steady-state decode allocates nothing).
///
/// `raw_boxes` is row-major `[num_anchors, POSE_REGRESSION_LEN]`;
/// `raw_scores` is `[num_anchors]`; both index-align with `anchors`.
/// Centre/keypoint offsets are relative to the anchor centre; sizes are
/// absolute (normalized), all divided by [`DETECTOR_SCALE`].
pub fn decode_pose_detections_into(
    raw_boxes: &[f32],
    raw_scores: &[f32],
    anchors: &[Anchor],
    score_threshold: f32,
    out: &mut Vec<PersonDetection>,
) {
    out.clear();
    for (i, anchor) in anchors.iter().enumerate() {
        let Some(&raw_score) = raw_scores.get(i) else {
            break;
        };
        let score = sigmoid(raw_score.clamp(-SCORE_CLIP, SCORE_CLIP));
        if score < score_threshold {
            continue;
        }
        let base = i * POSE_REGRESSION_LEN;
        let Some(reg) = raw_boxes.get(base..base + POSE_REGRESSION_LEN) else {
            break;
        };
        // Box: centre offset relative to the anchor centre, absolute size.
        let cx = reg[0] / DETECTOR_SCALE + anchor.cx;
        let cy = reg[1] / DETECTOR_SCALE + anchor.cy;
        let w = reg[2] / DETECTOR_SCALE;
        let h = reg[3] / DETECTOR_SCALE;
        let bbox = Rect {
            xmin: cx - w * 0.5,
            ymin: cy - h * 0.5,
            xmax: cx + w * 0.5,
            ymax: cy + h * 0.5,
        };
        let mut keypoints = [Vec2::ZERO; POSE_KEYPOINTS];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = Vec2::new(
                reg[4 + k * 2] / DETECTOR_SCALE + anchor.cx,
                reg[4 + k * 2 + 1] / DETECTOR_SCALE + anchor.cy,
            );
        }
        out.push(PersonDetection {
            score,
            bbox,
            keypoints,
        });
    }
}

/// Select the single primary person: the argmax-score detection, score-blended
/// with every detection whose IoU against it is ≥ `iou_threshold` (MediaPipe's
/// weighted-NMS blend restricted to the top cluster — see module docs).
/// Allocation-free. Returns `None` when `dets` is empty.
#[must_use]
pub fn best_person(dets: &[PersonDetection], iou_threshold: f32) -> Option<PersonDetection> {
    let seed = dets.iter().max_by(|a, b| a.score.total_cmp(&b.score))?;
    let mut total = 0.0_f32;
    let mut bbox = Rect {
        xmin: 0.0,
        ymin: 0.0,
        xmax: 0.0,
        ymax: 0.0,
    };
    let mut keypoints = [Vec2::ZERO; POSE_KEYPOINTS];
    for d in dets {
        if seed.bbox.iou(&d.bbox) < iou_threshold {
            continue;
        }
        // Weighted accumulate; normalized by `total` below.
        total += d.score;
        bbox.xmin += d.bbox.xmin * d.score;
        bbox.ymin += d.bbox.ymin * d.score;
        bbox.xmax += d.bbox.xmax * d.score;
        bbox.ymax += d.bbox.ymax * d.score;
        for (acc, kp) in keypoints.iter_mut().zip(d.keypoints.iter()) {
            *acc += *kp * d.score;
        }
    }
    if total <= 0.0 {
        // Degenerate scores: fall back to the seed verbatim.
        return Some(seed.clone());
    }
    let inv = 1.0 / total;
    bbox.xmin *= inv;
    bbox.ymin *= inv;
    bbox.xmax *= inv;
    bbox.ymax *= inv;
    for kp in &mut keypoints {
        *kp *= inv;
    }
    Some(PersonDetection {
        // The seed is the cluster maximum by construction.
        score: seed.score,
        bbox,
        keypoints,
    })
}

/// Lossless `u32` → `f32` for grid sizes/indices here (all ≤ 224).
fn grid_f32(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::detector`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): pose detector anchors, decode, and single-person selection" -m "SSD anchor grid for the 224x224 BlazePose detector (2254 anchors: strides 8/16/32x3, offsets 0.5, fixed anchor size), regression decode with scale 224 and score clip 100, and argmax-cluster weighted blend for the single-dancer case." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: ROI geometry and landmark projection (`roi.rs`)

The alignment-point ROI (detector keypoints AND aux landmark rows share one formula), the 39-row landmark decode/projection, and the `ContentRect` (adapted from the hand pipeline, plus the inverse mapping the mask warp needs).

**Files:**
- Create: `crates/wc-core/src/input/body/roi.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod roi;`)

**Interfaces:**
- Consumes: `detector::{PersonDetection, sigmoid}`, `bevy::math::{Vec2, Vec3}`.
- Produces: `RoiRect`, `roi_from_alignment_points`, `roi_from_detection`, `RawBodyLandmark`, `project_body_landmarks`, `roi_from_body_landmarks`, `roi_trackable`, `ContentRect`, `LANDMARK_INPUT = 256.0`, `LANDMARK_ROWS = 39`, `LANDMARK_VALUES = 5`, `AUX_CENTER_ROW = 33`, `AUX_SCALE_ROW = 34`, `MIN_TRACK_ROI_SIZE`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn alignment_roi_scales_and_centres_on_the_first_point() {
        // Scale point straight above the centre (image y grows downward, so
        // "up" is −y): rotation target is exactly met → 0.
        let roi = roi_from_alignment_points(Vec2::new(0.5, 0.6), Vec2::new(0.5, 0.4));
        assert!((roi.cx - 0.5).abs() < 1e-6);
        assert!((roi.cy - 0.6).abs() < 1e-6);
        // side = 2 × dist(0.2) × 1.25 = 0.5.
        assert!((roi.size - 0.5).abs() < 1e-5, "size={}", roi.size);
        assert!(roi.rotation.abs() < 1e-5, "rot={}", roi.rotation);
    }

    #[test]
    fn alignment_roi_rotates_a_sideways_body_upright() {
        // Scale point to the RIGHT of the centre (a person lying sideways):
        // the crop must rotate 90° to bring them upright.
        let roi = roi_from_alignment_points(Vec2::new(0.4, 0.5), Vec2::new(0.6, 0.5));
        assert!((roi.rotation - FRAC_PI_2).abs() < 1e-5, "rot={}", roi.rotation);
    }

    #[test]
    fn project_centre_landmark_maps_to_roi_centre() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let mut raw = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        raw[0] = LANDMARK_INPUT / 2.0; // x = 128 (crop centre)
        raw[1] = LANDMARK_INPUT / 2.0; // y = 128
        raw[3] = 10.0; // visibility logit → sigmoid ≈ 1
        raw[4] = -10.0; // presence logit → sigmoid ≈ 0
        let rows = project_body_landmarks(&raw, &roi);
        assert!((rows[0].pos.x - 0.5).abs() < 1e-5);
        assert!((rows[0].pos.y - 0.5).abs() < 1e-5);
        assert!(rows[0].visibility > 0.99);
        assert!(rows[0].presence < 0.01);
    }

    #[test]
    fn project_offset_landmark_scales_by_roi_size() {
        let roi = RoiRect {
            cx: 0.5,
            cy: 0.5,
            size: 0.4,
            rotation: 0.0,
        };
        let mut raw = [0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        // Crop x = 3/4 width → u = 0.25 → +0.25·0.4 = +0.1 → image x 0.6.
        raw[0] = LANDMARK_INPUT * 0.75;
        raw[1] = LANDMARK_INPUT / 2.0;
        let rows = project_body_landmarks(&raw, &roi);
        assert!((rows[0].pos.x - 0.6).abs() < 1e-5, "x={}", rows[0].pos.x);
        assert!((rows[0].pos.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn tracking_roi_comes_from_the_aux_rows() {
        let mut rows = [RawBodyLandmark::default(); LANDMARK_ROWS];
        rows[AUX_CENTER_ROW].pos = Vec3::new(0.5, 0.55, 0.0);
        rows[AUX_SCALE_ROW].pos = Vec3::new(0.5, 0.35, 0.0);
        let roi = roi_from_body_landmarks(&rows);
        assert!((roi.cx - 0.5).abs() < 1e-6);
        assert!((roi.cy - 0.55).abs() < 1e-6);
        assert!((roi.size - 0.5).abs() < 1e-5); // 2 × 0.2 × 1.25
        assert!(roi.rotation.abs() < 1e-5);
    }

    #[test]
    fn content_rect_strips_landscape_bars_and_round_trips() {
        // 1280×720 → square side 1280, bars top/bottom: y ∈ [0.21875, 0.78125].
        let content = ContentRect::for_frame(1280, 720);
        assert!((content.y0 - 0.21875).abs() < 1e-6);
        assert!((content.y1 - 0.78125).abs() < 1e-6);
        let p = content.to_content_norm(Vec3::new(0.5, 0.21875, 0.0));
        assert!((p.y - 0.0).abs() < 1e-6);
        // from_content_norm inverts to_content_norm.
        let sq = content.from_content_norm(0.25, 0.75);
        let back = content.to_content_norm(Vec3::new(sq.x, sq.y, 0.0));
        assert!((back.x - 0.25).abs() < 1e-6 && (back.y - 0.75).abs() < 1e-6);
    }

    #[test]
    fn roi_trackable_rejects_offscreen_tiny_and_nonfinite() {
        let content = ContentRect::for_frame(64, 64);
        let ok = RoiRect { cx: 0.5, cy: 0.5, size: 0.4, rotation: 0.0 };
        assert!(roi_trackable(&ok, content));
        let tiny = RoiRect { size: 0.01, ..ok };
        assert!(!roi_trackable(&tiny, content));
        let offscreen = RoiRect { cx: 1.4, ..ok };
        assert!(!roi_trackable(&offscreen, content));
        let bad = RoiRect { size: f32::NAN, ..ok };
        assert!(!roi_trackable(&bad, content));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::roi`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod roi;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/roi.rs`:

```rust
//! ROI geometry for the two-stage BlazePose pipeline: build the rotated
//! person crop from detector alignment keypoints (or the previous frame's aux
//! landmarks — detect-then-track), and project the landmark model's
//! crop-space output back to square-normalized image space.
//!
//! MediaPipe's `AlignmentPointsRectsCalculator` defines the person box by two
//! alignment points: the box centre (detector keypoint 0 = mid-hip; aux
//! landmark row 33 when tracking) and a point on the circle circumscribing
//! the box (keypoint 1 / aux row 34), so the square side is twice their
//! distance. `RectTransformationCalculator` then expands by 1.25× (both
//! `pose_detection_to_roi.pbtxt` and `pose_landmarks_to_roi.pbtxt` use
//! `scale 1.25, square_long`), and the rotation brings the centre→scale-point
//! vector to vertical (target 90°).
//!
//! Coordinate spaces mirror the hand pipeline: the models run in
//! **square-norm** `[0, 1]²` (the square-padded camera frame); publication
//! converts to **content-norm** (padding bars stripped — the pinned "mask UV
//! space"), via [`ContentRect`].

use std::f32::consts::FRAC_PI_2;

use bevy::math::{Vec2, Vec3};

use super::detector::{sigmoid, PersonDetection};

/// ROI expansion factor (`RectTransformationCalculator scale_x/y: 1.25`).
pub const ROI_EXPANSION: f32 = 1.25;

/// Side length the landmark model consumes (256×256).
pub const LANDMARK_INPUT: f32 = 256.0;

/// Rows in the landmark tensor: 33 published landmarks + 2 aux tracking
/// alignment points (rows 33/34) + 4 unused rows.
pub const LANDMARK_ROWS: usize = 39;

/// Values per landmark row: x, y, z, visibility logit, presence logit.
pub const LANDMARK_VALUES: usize = 5;

/// Aux row holding the tracking ROI centre.
pub const AUX_CENTER_ROW: usize = 33;

/// Aux row holding the tracking ROI circumscribing-circle point.
pub const AUX_SCALE_ROW: usize = 34;

/// Smallest landmark-derived ROI still plausible as a track (normalized
/// square units). When the person leaves the camera the aux points can
/// collapse together while presence stays high on a clamped edge crop; size
/// is the signal the track is unusable — drop it and re-detect.
pub const MIN_TRACK_ROI_SIZE: f32 = 0.05;

/// A rotated square region of interest in normalized image coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoiRect {
    /// Centre x in `[0, 1]`.
    pub cx: f32,
    /// Centre y in `[0, 1]`.
    pub cy: f32,
    /// Side length (square) in normalized units.
    pub size: f32,
    /// Rotation in radians (CCW) aligning the centre→scale-point axis to
    /// vertical.
    pub rotation: f32,
}

/// Build the person ROI from two alignment points (see module docs):
/// centre = `center`, side = `2·|scale_point − center|·`[`ROI_EXPANSION`],
/// rotation brings the centre→scale-point vector to vertical (target 90°,
/// same convention as the hand pipeline's `roi_from_palm`).
#[must_use]
pub fn roi_from_alignment_points(center: Vec2, scale_point: Vec2) -> RoiRect {
    let d = scale_point - center;
    let rotation = FRAC_PI_2 - (-d.y).atan2(d.x);
    RoiRect {
        cx: center.x,
        cy: center.y,
        size: 2.0 * d.length() * ROI_EXPANSION,
        rotation,
    }
}

/// Person ROI from a detector hit: keypoint 0 (mid-hip) is the centre,
/// keypoint 1 (full-body circumscribing point) the scale/rotation reference.
#[must_use]
pub fn roi_from_detection(det: &PersonDetection) -> RoiRect {
    roi_from_alignment_points(det.keypoints[0], det.keypoints[1])
}

/// One decoded landmark row in square-normalized image space, with its
/// activated visibility/presence probabilities.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawBodyLandmark {
    /// Square-norm position; `z` is the model's relative depth scaled by the
    /// ROI size (coarse, not metric).
    pub pos: Vec3,
    /// Visibility probability in `[0, 1]` (sigmoid of the raw logit).
    pub visibility: f32,
    /// Presence probability in `[0, 1]` (sigmoid of the raw logit).
    pub presence: f32,
}

/// Project the landmark model's `[195]` crop-space output back to
/// square-normalized image coordinates (inverse of the ROI warp, mirroring
/// the hand pipeline's `project_landmarks`), activating the visibility and
/// presence logits. Returns a stack array — no allocation on the frame path.
#[must_use]
pub fn project_body_landmarks(raw: &[f32], roi: &RoiRect) -> [RawBodyLandmark; LANDMARK_ROWS] {
    let (sin, cos) = roi.rotation.sin_cos();
    let mut out = [RawBodyLandmark::default(); LANDMARK_ROWS];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * LANDMARK_VALUES;
        let lx = raw.get(base).copied().unwrap_or(0.0);
        let ly = raw.get(base + 1).copied().unwrap_or(0.0);
        let lz = raw.get(base + 2).copied().unwrap_or(0.0);
        let vis = raw.get(base + 3).copied().unwrap_or(0.0);
        let pres = raw.get(base + 4).copied().unwrap_or(0.0);
        // Crop pixel → centred unit → scaled by ROI size → rotated → translated.
        let u = (lx / LANDMARK_INPUT - 0.5) * roi.size;
        let v = (ly / LANDMARK_INPUT - 0.5) * roi.size;
        lm.pos = Vec3::new(
            roi.cx + u * cos - v * sin,
            roi.cy + u * sin + v * cos,
            lz / LANDMARK_INPUT * roi.size,
        );
        lm.visibility = sigmoid(vis);
        lm.presence = sigmoid(pres);
    }
    out
}

/// Next-frame tracking ROI from this frame's aux alignment rows (33 centre,
/// 34 scale point) — MediaPipe's `pose_landmarks_to_roi` path, letting
/// tracking frames skip the detector entirely.
#[must_use]
pub fn roi_from_body_landmarks(rows: &[RawBodyLandmark; LANDMARK_ROWS]) -> RoiRect {
    roi_from_alignment_points(
        rows[AUX_CENTER_ROW].pos.truncate(),
        rows[AUX_SCALE_ROW].pos.truncate(),
    )
}

/// True if a landmark-derived ROI is worth carrying into the next frame:
/// centre still inside the camera content (not drifted into a padding bar),
/// finite, and at least [`MIN_TRACK_ROI_SIZE`].
#[must_use]
pub fn roi_trackable(roi: &RoiRect, content: ContentRect) -> bool {
    content.contains(roi.cx, roi.cy) && roi.size.is_finite() && roi.size >= MIN_TRACK_ROI_SIZE
}

/// The camera content rectangle inside the square-padded image, in
/// square-normalized coordinates (adapted from the hand pipeline — see its
/// `ContentRect` for the full rationale: padding bars live *inside* `[0, 1]²`
/// of the square, so bare range tests treat an off-camera person as
/// on-screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentRect {
    /// Left edge (square-norm).
    pub x0: f32,
    /// Top edge (square-norm).
    pub y0: f32,
    /// Right edge (square-norm).
    pub x1: f32,
    /// Bottom edge (square-norm).
    pub y1: f32,
}

impl ContentRect {
    /// Content rect for a `frame_w × frame_h` camera frame square-padded to
    /// its larger side (origin-centred padding, matching `square_pad_into`).
    #[must_use]
    pub fn for_frame(frame_w: u32, frame_h: u32) -> Self {
        let side = frame_w.max(frame_h).max(1);
        let sidef = dim(side);
        let ox = dim((side - frame_w) / 2);
        let oy = dim((side - frame_h) / 2);
        Self {
            x0: ox / sidef,
            y0: oy / sidef,
            x1: (ox + dim(frame_w)) / sidef,
            y1: (oy + dim(frame_h)) / sidef,
        }
    }

    /// Map a square-normalized point into content-normalized coordinates
    /// (`x' = (x − x0)/(x1 − x0)`, `y'` analog, `z` passes through). This is
    /// the publication step that makes landmark xy live in mask UV space.
    ///
    /// # Invariant
    /// `for_frame` enforces non-zero frame dims, so `x1 > x0` and `y1 > y0`;
    /// the divisions are safe (debug-asserted).
    #[must_use]
    pub fn to_content_norm(self, p: Vec3) -> Vec3 {
        let w = self.x1 - self.x0;
        let h = self.y1 - self.y0;
        debug_assert!(w > 0.0, "content rect has zero width: {self:?}");
        debug_assert!(h > 0.0, "content rect has zero height: {self:?}");
        Vec3::new((p.x - self.x0) / w, (p.y - self.y0) / h, p.z)
    }

    /// Inverse of [`Self::to_content_norm`] for a 2-D point: map
    /// content-normalized `(u, v)` back into square-normalized coordinates.
    /// The mask warp iterates output texels in content space and needs their
    /// square-norm position to invert the ROI transform.
    #[must_use]
    pub fn from_content_norm(self, u: f32, v: f32) -> Vec2 {
        Vec2::new(
            self.x0 + u * (self.x1 - self.x0),
            self.y0 + v * (self.y1 - self.y0),
        )
    }

    /// Whether the square-normalized point `(cx, cy)` lies within the content.
    #[must_use]
    pub fn contains(self, cx: f32, cy: f32) -> bool {
        (self.x0..=self.x1).contains(&cx) && (self.y0..=self.y1).contains(&cy)
    }
}

/// `u32` → `f32` for image dimensions (≤ 65535 for realistic frames).
fn dim(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::roi`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): alignment-point ROI geometry and 39-row landmark projection" -m "Shared alignment-points formula (centre, 2x distance, 1.25x expansion, 90-degree rotation target) for detector keypoints and aux tracking rows; crop-to-square-norm projection with sigmoid-activated visibility/presence; ContentRect with the inverse mapping the mask warp needs." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: Mask post-processing (`mask.rs`)

Sigmoid the crop-space mask logits, inverse-warp them into the content-normalized frame grid, EMA them over time (worker-side, suppressing mask flicker), and quantize to the pooled u8 buffer.

**Files:**
- Create: `crates/wc-core/src/input/body/mask.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod mask;`)

**Interfaces:**
- Consumes: `roi::{ContentRect, RoiRect, LANDMARK_INPUT}`, `detector::sigmoid`, `MASK_SIZE`.
- Produces: `MaskProcessor` (`new`, `ingest`, `decay`, `smoothed`, `write_u8`, `reset`), `ema_blend`, `ema_decay`, `DEFAULT_MASK_EMA_ALPHA` — consumed by Task 9.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_blend_with_alpha_one_copies_the_target() {
        let mut acc = vec![0.0_f32; 4];
        ema_blend(&mut acc, &[1.0, 0.5, 0.25, 0.0], 1.0);
        assert_eq!(acc, vec![1.0, 0.5, 0.25, 0.0]);
    }

    #[test]
    fn ema_blend_converges_geometrically_on_a_step() {
        // acc starts 0, target 1, alpha 0.5: after n blends acc = 1 − 0.5^n.
        let mut acc = vec![0.0_f32];
        for n in 1..=8 {
            ema_blend(&mut acc, &[1.0], 0.5);
            let expected = 1.0 - 0.5_f32.powi(n);
            assert!((acc[0] - expected).abs() < 1e-6, "n={n} acc={}", acc[0]);
        }
    }

    #[test]
    fn ema_decay_fades_toward_zero() {
        let mut acc = vec![1.0_f32];
        ema_decay(&mut acc, 0.5);
        assert!((acc[0] - 0.5).abs() < 1e-6);
        for _ in 0..30 {
            ema_decay(&mut acc, 0.5);
        }
        assert!(acc[0] < 1e-3);
    }

    /// Full-content identity-ish setup: a square "camera" frame so the
    /// content rect is the whole square, and an ROI covering the whole frame
    /// unrotated — the warp becomes (approximately) the identity.
    fn identity_setup() -> (ContentRect, RoiRect) {
        (
            ContentRect::for_frame(256, 256),
            RoiRect {
                cx: 0.5,
                cy: 0.5,
                size: 1.0,
                rotation: 0.0,
            },
        )
    }

    #[test]
    fn first_ingest_seeds_the_ema_without_history_lag() {
        let (content, roi) = identity_setup();
        // Strongly-positive logits everywhere → sigmoid ≈ 1 across the crop.
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 0.25);
        // First frame copies (no EMA lag from the zero-initialized history).
        let centre = p.smoothed()[(MASK_SIZE / 2) * MASK_SIZE + MASK_SIZE / 2];
        assert!(centre > 0.99, "centre={centre}");
    }

    #[test]
    fn ingest_warps_a_centred_blob_to_the_frame_centre() {
        let (content, roi) = identity_setup();
        // Person square in crop pixels [96, 160)²: +8 logits inside, −8 out.
        let mut logits = vec![-8.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 96..160 {
            for x in 96..160 {
                logits[y * MASK_SIZE + x] = 8.0;
            }
        }
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let m = p.smoothed();
        let centre = m[128 * MASK_SIZE + 128];
        let corner = m[4 * MASK_SIZE + 4];
        assert!(centre > 0.9, "centre={centre}");
        assert!(corner < 0.1, "corner={corner}");
    }

    #[test]
    fn pixels_outside_the_roi_read_zero() {
        let content = ContentRect::for_frame(256, 256);
        // Tiny ROI in the upper-left: everything far from it must be 0 even
        // though the crop itself is fully "person".
        let roi = RoiRect {
            cx: 0.2,
            cy: 0.2,
            size: 0.2,
            rotation: 0.0,
        };
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let m = p.smoothed();
        assert!(m[240 * MASK_SIZE + 240] < 1e-6, "far corner must be empty");
        // ROI centre (0.2, 0.2) ≈ texel (51, 51) on the 256 grid.
        let inside = m[51 * MASK_SIZE + 51];
        assert!(inside > 0.9, "roi interior={inside}");
    }

    #[test]
    fn write_u8_quantizes_the_full_range() {
        let (content, roi) = identity_setup();
        let logits = vec![10.0_f32; MASK_SIZE * MASK_SIZE];
        let mut p = MaskProcessor::new();
        p.ingest(&logits, &roi, content, 1.0);
        let mut out = vec![0_u8; MASK_SIZE * MASK_SIZE];
        p.write_u8(&mut out);
        assert_eq!(out[128 * MASK_SIZE + 128], 255);
        p.reset();
        p.write_u8(&mut out);
        assert_eq!(out[128 * MASK_SIZE + 128], 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::mask`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod mask;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/mask.rs`:

```rust
//! Segmentation-mask post-processing (worker-side): sigmoid the landmark
//! model's crop-space mask logits, inverse-warp them into the 256×256
//! content-normalized frame grid (the pinned "mask UV space" shared with the
//! published landmarks), EMA over time to suppress frame-to-frame mask
//! flicker, and quantize into the pooled `u8` buffer.
//!
//! All three working buffers (crop, frame, EMA accumulator — 256 KB of `f32`
//! each) are allocated once in [`MaskProcessor::new`] and refilled in place:
//! the per-frame path performs no allocation (worker-loop hot-path rule).

use super::detector::sigmoid;
use super::roi::{ContentRect, RoiRect, LANDMARK_INPUT};
use super::MASK_SIZE;

/// Default temporal EMA factor for the mask (fraction of the new frame
/// blended in per body frame). 0.35 bridges single-frame mask dropouts while
/// keeping ~3-frame latency on silhouette changes; live-tunable through
/// `BodyLiveTuning` (Plan C's dev panel binds it).
pub const DEFAULT_MASK_EMA_ALPHA: f32 = 0.35;

/// Blend `target` into `acc`: `acc += (target − acc) · alpha` per element.
/// `alpha` is clamped to `[0, 1]`.
pub fn ema_blend(acc: &mut [f32], target: &[f32], alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    for (acc, t) in acc.iter_mut().zip(target) {
        *acc += (t - *acc) * a;
    }
}

/// Decay `acc` toward zero: `acc −= acc · alpha` per element (the
/// person-absent mask fade). `alpha` is clamped to `[0, 1]`.
pub fn ema_decay(acc: &mut [f32], alpha: f32) {
    let a = alpha.clamp(0.0, 1.0);
    for v in acc.iter_mut() {
        *v -= *v * a;
    }
}

/// Owns the mask working buffers and the temporal EMA state.
pub struct MaskProcessor {
    /// Sigmoid-activated crop-space mask (`MASK_SIZE`², refilled per frame).
    crop: Vec<f32>,
    /// Frame-space (content-norm) warped mask for the current frame.
    frame: Vec<f32>,
    /// Temporal EMA accumulator — what consumers see via [`Self::smoothed`].
    ema: Vec<f32>,
    /// Whether `ema` holds real history (first frame copies instead of
    /// blending, so a fresh track has no fade-in lag from the zero state).
    has_history: bool,
}

impl MaskProcessor {
    /// Allocate the three working buffers (the only allocation this type
    /// ever performs).
    #[must_use]
    pub fn new() -> Self {
        Self {
            crop: vec![0.0; MASK_SIZE * MASK_SIZE],
            frame: vec![0.0; MASK_SIZE * MASK_SIZE],
            ema: vec![0.0; MASK_SIZE * MASK_SIZE],
            has_history: false,
        }
    }

    /// Forget all mask state (track lost / worker restart).
    pub fn reset(&mut self) {
        self.ema.fill(0.0);
        self.has_history = false;
    }

    /// Ingest one crop-space mask: sigmoid `mask_logits` (row-major
    /// `MASK_SIZE`², the landmark model's `[1, 256, 256, 1]` output),
    /// inverse-warp through `roi`/`content` into frame space, and EMA-blend
    /// with factor `alpha`. Extra/short input is clamped defensively (the
    /// pipeline validates the tensor shape before calling).
    pub fn ingest(&mut self, mask_logits: &[f32], roi: &RoiRect, content: ContentRect, alpha: f32) {
        // 1. Activate the crop (65 k sigmoids, trivially cheap next to the
        //    model itself).
        for (dst, logit) in self.crop.iter_mut().zip(mask_logits) {
            *dst = sigmoid(*logit);
        }
        // 2. Inverse-warp: for each frame texel, find its square-norm
        //    position, rotate/scale into the crop's upright frame, and
        //    bilinearly sample the crop (0 outside — no person beyond the ROI).
        let (sin, cos) = roi.rotation.sin_cos();
        let inv_size = if roi.size > 0.0 { 1.0 / roi.size } else { 0.0 };
        let n = cellf(MASK_SIZE);
        for y in 0..MASK_SIZE {
            let v = (cellf(y) + 0.5) / n;
            for x in 0..MASK_SIZE {
                let u = (cellf(x) + 0.5) / n;
                let sq = content.from_content_norm(u, v);
                let dx = sq.x - roi.cx;
                let dy = sq.y - roi.cy;
                // Rotate by −rotation (transpose) into the crop frame.
                let cu = dx * cos + dy * sin;
                let cv = -dx * sin + dy * cos;
                let px = (cu * inv_size + 0.5) * LANDMARK_INPUT;
                let py = (cv * inv_size + 0.5) * LANDMARK_INPUT;
                self.frame[y * MASK_SIZE + x] = if inv_size > 0.0
                    && px >= 0.0
                    && px < LANDMARK_INPUT
                    && py >= 0.0
                    && py < LANDMARK_INPUT
                {
                    sample_bilinear(&self.crop, px, py)
                } else {
                    0.0
                };
            }
        }
        // 3. Temporal EMA (first frame copies — no fade-in lag).
        if self.has_history {
            ema_blend(&mut self.ema, &self.frame, alpha);
        } else {
            self.ema.copy_from_slice(&self.frame);
            self.has_history = true;
        }
    }

    /// Fade the mask toward empty (called on person-absent frames so a stale
    /// silhouette never lingers). No-op before the first ingest.
    pub fn decay(&mut self, alpha: f32) {
        if self.has_history {
            ema_decay(&mut self.ema, alpha);
        }
    }

    /// The EMA-smoothed frame-space mask (`MASK_SIZE`² values in `[0, 1]`) —
    /// the edge extractor's input.
    #[must_use]
    pub fn smoothed(&self) -> &[f32] {
        &self.ema
    }

    /// Quantize the smoothed mask into a `R8Unorm` byte buffer (the pooled
    /// payload written in place — no allocation).
    pub fn write_u8(&self, out: &mut [u8]) {
        for (dst, &v) in out.iter_mut().zip(&self.ema) {
            *dst = byte(v * 255.0);
        }
    }
}

impl Default for MaskProcessor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bilinear sample of a `MASK_SIZE`² scalar grid at continuous index
/// coordinates, clamped to the edge (same convention as the hand pipeline's
/// RGB `sample_bilinear`).
fn sample_bilinear(m: &[f32], x: f32, y: f32) -> f32 {
    let max = cellf(MASK_SIZE - 1);
    let xc = x.clamp(0.0, max);
    let yc = y.clamp(0.0, max);
    let fx = xc - xc.floor();
    let fy = yc - yc.floor();
    let x0 = floor_index(xc);
    let y0 = floor_index(yc);
    let x1 = (x0 + 1).min(MASK_SIZE - 1);
    let y1 = (y0 + 1).min(MASK_SIZE - 1);
    let p00 = m[y0 * MASK_SIZE + x0];
    let p10 = m[y0 * MASK_SIZE + x1];
    let p01 = m[y1 * MASK_SIZE + x0];
    let p11 = m[y1 * MASK_SIZE + x1];
    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    top + (bot - top) * fy
}

/// `usize` → `f32` for mask-grid indices (all ≤ 256, exact in `f32`).
fn cellf(v: usize) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Floor a finite, clamped, grid-bounded float to a mask index.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is finite, clamped >= 0, and bounded by MASK_SIZE; float->int has no From/TryFrom"
)]
fn floor_index(v: f32) -> usize {
    v.max(0.0).floor() as usize
}

/// Round a `[0, 255]`-clamped float to a mask byte.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255]; float->int has no From/TryFrom"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::mask`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): mask crop-to-frame warp, temporal EMA, and u8 quantization" -m "MaskProcessor sigmoids the crop-space logits, inverse-warps them into the content-normalized 256x256 frame grid shared with the published landmarks, EMAs over time (first frame copies, absent frames decay), and quantizes into the pooled byte buffer. All buffers init-allocated." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 7: Silhouette edge extraction (`edges.rs`)

Single-pass scan of the smoothed mask for 0.5-crossings between neighbouring texels, emitting `(position, outward normal)` pairs into a fixed-capacity buffer.

**Files:**
- Create: `crates/wc-core/src/input/body/edges.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod edges;`)

**Interfaces:**
- Consumes: `EdgePoint`, `MASK_SIZE`, `MAX_EDGE_POINTS`, `bevy::math::Vec2`.
- Produces: `extract_edges(mask: &[f32], out: &mut Vec<EdgePoint>)`, `EDGE_THRESHOLD` — consumed by Task 9.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec2;

    /// Binary disc mask: 1.0 inside radius `r` texels of `centre`, else 0.0.
    fn disc(centre: Vec2, r: f32) -> Vec<f32> {
        let mut m = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                let p = Vec2::new(cellf(x) + 0.5, cellf(y) + 0.5);
                if p.distance(centre) < r {
                    m[y * MASK_SIZE + x] = 1.0;
                }
            }
        }
        m
    }

    fn cellf(v: usize) -> f32 {
        u16::try_from(v).map(f32::from).unwrap_or(0.0)
    }

    #[test]
    fn circle_yields_perimeter_points_with_outward_unit_normals() {
        let centre = Vec2::new(128.0, 128.0);
        let mask = disc(centre, 60.0);
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut out);
        // A radius-60 disc crosses ~2 texels per row over ~120 rows plus the
        // same per column: ≈ 480 crossings. Wide band for discretization.
        assert!(
            (380..=600).contains(&out.len()),
            "unexpected edge count {}",
            out.len()
        );
        let centre_uv = centre / cellf(MASK_SIZE);
        for p in &out {
            // Unit-length normal…
            assert!((p.normal.length() - 1.0).abs() < 1e-3, "normal={:?}", p.normal);
            // …pointing away from the disc centre (outward).
            let radial = p.pos - centre_uv;
            assert!(
                radial.dot(p.normal) > 0.0,
                "normal {:?} not outward at {:?}",
                p.normal,
                p.pos
            );
            // Positions stay in the unit square, on the circle (± one texel).
            let r_uv = 60.0 / cellf(MASK_SIZE);
            assert!((radial.length() - r_uv).abs() < 2.0 / cellf(MASK_SIZE));
        }
    }

    #[test]
    fn torso_blob_edges_have_axis_aligned_normals_on_the_flanks() {
        // A filled axis-aligned rectangle (torso stand-in): x ∈ [96, 160),
        // y ∈ [64, 192).
        let mut mask = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 64..192 {
            for x in 96..160 {
                mask[y * MASK_SIZE + x] = 1.0;
            }
        }
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask, &mut out);
        // 2 horizontal crossings × 128 rows + 2 vertical × 64 columns = 384.
        assert!(
            (350..=420).contains(&out.len()),
            "unexpected edge count {}",
            out.len()
        );
        // Points on the left flank (x ≈ 96/256, away from corners) must point
        // straight −x.
        let mut checked = 0;
        for p in &out {
            if (p.pos.x - 96.0 / 256.0).abs() < 1.5 / 256.0
                && p.pos.y > 100.0 / 256.0
                && p.pos.y < 150.0 / 256.0
            {
                assert!(p.normal.x < -0.9, "left-flank normal {:?}", p.normal);
                assert!(p.normal.y.abs() < 0.3);
                checked += 1;
            }
        }
        assert!(checked > 10, "too few left-flank samples: {checked}");
    }

    #[test]
    fn capacity_clamps_without_reallocating() {
        // Vertical stripes: every horizontal neighbour pair crosses 0.5 —
        // ~255 crossings per row × 256 rows, far beyond MAX_EDGE_POINTS.
        let mut mask = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 0..MASK_SIZE {
            for x in 0..MASK_SIZE {
                if x % 2 == 0 {
                    mask[y * MASK_SIZE + x] = 1.0;
                }
            }
        }
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        let ptr = out.as_ptr();
        extract_edges(&mask, &mut out);
        assert_eq!(out.len(), MAX_EDGE_POINTS, "must clamp at capacity");
        assert_eq!(out.capacity(), MAX_EDGE_POINTS, "must never grow");
        assert_eq!(out.as_ptr(), ptr, "must never reallocate");
    }

    #[test]
    fn refill_clears_previous_points() {
        let mask_a = disc(Vec2::new(128.0, 128.0), 40.0);
        let empty = vec![0.0_f32; MASK_SIZE * MASK_SIZE];
        let mut out = Vec::with_capacity(MAX_EDGE_POINTS);
        extract_edges(&mask_a, &mut out);
        assert!(!out.is_empty());
        extract_edges(&empty, &mut out);
        assert!(out.is_empty(), "clear-refill semantics");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::edges`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod edges;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/edges.rs`:

```rust
//! Silhouette edge extraction: scan the EMA-smoothed mask for
//! [`EDGE_THRESHOLD`] crossings between neighbouring texels and emit up to
//! [`MAX_EDGE_POINTS`] `(position, outward normal)` pairs.
//!
//! Runs on the worker (a single 256² pass, negligible next to inference).
//! The output is Plan C's particle-emission surface (uploaded as a storage
//! buffer) and doubles as the silhouette rim source. The caller supplies a
//! buffer with capacity [`MAX_EDGE_POINTS`]; extraction clear-refills it and
//! clamps at capacity, so it never allocates (worker hot-path rule).

use bevy::math::Vec2;

use super::{EdgePoint, MASK_SIZE, MAX_EDGE_POINTS};

/// Iso-level at which the mask boundary is traced.
pub const EDGE_THRESHOLD: f32 = 0.5;

/// Extract silhouette edge points from a `MASK_SIZE`² smoothed mask
/// (row-major, values in `[0, 1]`) into `out` (cleared first; capacity must
/// be ≥ [`MAX_EDGE_POINTS`], which the pooled payload and `SilhouetteEdges`
/// guarantee by construction).
///
/// Two passes in deterministic scan order: horizontal crossings (between
/// x and x+1) then vertical (between y and y+1). Each crossing interpolates
/// the sub-texel position and takes the outward normal from the mask
/// gradient (central differences, clamped at borders): inside > threshold >
/// outside, so the outward direction is −gradient. Degenerate zero-gradient
/// crossings are skipped rather than given a fake normal.
pub fn extract_edges(mask: &[f32], out: &mut Vec<EdgePoint>) {
    out.clear();
    debug_assert_eq!(mask.len(), MASK_SIZE * MASK_SIZE);
    let n = MASK_SIZE;
    let nf = cellf(n);
    // Horizontal crossings: between (x, y) and (x+1, y).
    for y in 0..n {
        for x in 0..n - 1 {
            if out.len() == MAX_EDGE_POINTS {
                return;
            }
            let a = mask[y * n + x];
            let b = mask[y * n + x + 1];
            if !crosses(a, b) {
                continue;
            }
            let t = (EDGE_THRESHOLD - a) / (b - a);
            let pos = Vec2::new((cellf(x) + 0.5 + t) / nf, (cellf(y) + 0.5) / nf);
            let sample_x = if t < 0.5 { x } else { x + 1 };
            if let Some(normal) = outward_normal(mask, sample_x, y) {
                out.push(EdgePoint { pos, normal });
            }
        }
    }
    // Vertical crossings: between (x, y) and (x, y+1).
    for y in 0..n - 1 {
        for x in 0..n {
            if out.len() == MAX_EDGE_POINTS {
                return;
            }
            let a = mask[y * n + x];
            let b = mask[(y + 1) * n + x];
            if !crosses(a, b) {
                continue;
            }
            let t = (EDGE_THRESHOLD - a) / (b - a);
            let pos = Vec2::new((cellf(x) + 0.5) / nf, (cellf(y) + 0.5 + t) / nf);
            let sample_y = if t < 0.5 { y } else { y + 1 };
            if let Some(normal) = outward_normal(mask, x, sample_y) {
                out.push(EdgePoint { pos, normal });
            }
        }
    }
}

/// Whether the mask value crosses [`EDGE_THRESHOLD`] between two texels.
/// Strict inequality: a texel exactly at the threshold is not a crossing on
/// its own (its neighbour pair on the other side will be).
fn crosses(a: f32, b: f32) -> bool {
    (a - EDGE_THRESHOLD) * (b - EDGE_THRESHOLD) < 0.0
}

/// Outward unit normal at texel `(x, y)`: −normalize(∇mask), central
/// differences with border clamping. `None` when the local gradient is
/// degenerate (flat plateau — cannot orient a normal).
fn outward_normal(mask: &[f32], x: usize, y: usize) -> Option<Vec2> {
    let n = MASK_SIZE;
    let xl = x.saturating_sub(1);
    let xr = (x + 1).min(n - 1);
    let yu = y.saturating_sub(1);
    let yd = (y + 1).min(n - 1);
    let g = Vec2::new(
        mask[y * n + xr] - mask[y * n + xl],
        mask[yd * n + x] - mask[yu * n + x],
    );
    let len = g.length();
    if len > f32::EPSILON {
        Some(-g / len)
    } else {
        None
    }
}

/// `usize` → `f32` for mask-grid indices (all ≤ 256, exact in `f32`).
fn cellf(v: usize) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::edges`
Expected: PASS (4 tests). If the circle/torso count windows miss by discretization detail, widen the asserted range — the invariants that must NOT be weakened are outward orientation, unit length, capacity clamp, and pointer stability.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): silhouette edge extraction with fixed-capacity clamp" -m "Single-pass 0.5-crossing scan over the smoothed mask emitting (position, outward normal) pairs; normals from the local mask gradient; deterministic order; clamped at MAX_EDGE_POINTS with clear-refill semantics and zero allocation." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 8: Ring transport and the recycled payload pool (`transport.rs`)

The message enum, the frame struct, and the `Box<BodyFramePayload>` two-ring pool that carries the 256 KB mask + edge list without steady-state allocation.

**Files:**
- Create: `crates/wc-core/src/input/body/transport.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod transport;`)

**Interfaces:**
- Consumes: `BodyLandmark`, `BodyTrackingStatus`, `EdgePoint`, constants; `rtrb`.
- Produces: `BodyFramePayload`, `BodyFrame`, `BodyWorkerMsg`, `BodyWorkerDiagnostics`, `PAYLOAD_POOL_SIZE`, `RESULT_RING_CAPACITY`, `seed_payload_pool` — consumed by Tasks 9/11/12.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn payload_preallocates_mask_and_edge_capacity() {
        let p = BodyFramePayload::new();
        assert_eq!(p.mask.len(), MASK_SIZE * MASK_SIZE);
        assert!(p.edges.is_empty());
        assert_eq!(p.edges.capacity(), MAX_EDGE_POINTS);
    }

    #[test]
    fn pool_round_trip_reuses_the_same_buffers() {
        // The steady-state contract: after seeding, the same PAYLOAD_POOL_SIZE
        // heap buffers cycle worker→main→worker forever — no new allocation.
        let (mut recycle_tx, mut recycle_rx) =
            rtrb::RingBuffer::<Box<BodyFramePayload>>::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);

        let mut seen = std::collections::HashSet::new();
        for cycle in 0..(PAYLOAD_POOL_SIZE * 5) {
            // "Worker": claim a payload, fill it, hand it to "main".
            let mut payload = recycle_rx.pop().expect("pool never runs dry in lockstep");
            seen.insert(payload.mask.as_ptr());
            payload.mask[0] = u8::try_from(cycle % 256).expect("bounded");
            payload.edges.clear();
            // "Main": consume, then recycle.
            recycle_tx.push(payload).expect("recycle ring never full");
        }
        assert_eq!(
            seen.len(),
            PAYLOAD_POOL_SIZE,
            "exactly the seeded buffers must circulate"
        );
    }

    #[test]
    fn pool_exhaustion_is_observable_not_blocking() {
        let (mut recycle_tx, mut recycle_rx) =
            rtrb::RingBuffer::<Box<BodyFramePayload>>::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);
        // Drain the whole pool (main thread stalled, nothing recycled)…
        for _ in 0..PAYLOAD_POOL_SIZE {
            let _held = recycle_rx.pop().expect("seeded");
        }
        // …the next claim reports empty instead of blocking or allocating.
        assert!(recycle_rx.pop().is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::transport`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod transport;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/transport.rs`:

```rust
//! Worker↔main transport: the result-ring message enum and the recycled
//! mask/edge payload pool.
//!
//! Landmarks and status cross the result ring as plain values. The 256 KB
//! mask (+ up to 32 KB of edge points) rides in a [`BodyFramePayload`] `Box`
//! cycled through TWO rings: worker→main inside [`BodyWorkerMsg::Frame`]
//! (a pointer move, no copy), main→worker on a dedicated recycle ring after
//! the main thread has copied the bytes out. [`PAYLOAD_POOL_SIZE`] boxes are
//! allocated once at start ([`seed_payload_pool`]); steady state allocates
//! nothing (AGENTS.md hot-path rule — the worker loop is a hot path). If the
//! pool is momentarily dry (main thread stalled), the worker simply emits a
//! payload-less frame: landmarks stay fresh, the mask update skips a frame.

use std::time::Duration;

use bevy::math::Vec3;
use rtrb::Producer;

use super::pipeline::PoseDiagnostics;
use super::{
    BodyLandmark, BodyTrackingStatus, EdgePoint, BODY_LANDMARK_COUNT, MASK_SIZE, MAX_EDGE_POINTS,
};

/// Number of pooled mask/edge payloads: one in flight at the worker, one in
/// the result ring, one being consumed on the main thread.
pub const PAYLOAD_POOL_SIZE: usize = 3;

/// Result-ring depth (messages, not frames — status/diagnostics ride along).
pub const RESULT_RING_CAPACITY: usize = 64;

/// A pooled mask + edge-list buffer, reused for the life of the worker.
pub struct BodyFramePayload {
    /// `MASK_SIZE`² `R8Unorm` bytes, written in place by the mask processor.
    pub mask: Vec<u8>,
    /// Edge points for this frame (capacity [`MAX_EDGE_POINTS`], clear-refilled).
    pub edges: Vec<EdgePoint>,
}

impl BodyFramePayload {
    /// Allocate one payload (called only while seeding the pool).
    #[must_use]
    pub fn new() -> Self {
        Self {
            mask: vec![0; MASK_SIZE * MASK_SIZE],
            edges: Vec::with_capacity(MAX_EDGE_POINTS),
        }
    }
}

impl Default for BodyFramePayload {
    fn default() -> Self {
        Self::new()
    }
}

/// Seed the recycle ring with [`PAYLOAD_POOL_SIZE`] fresh payloads — the only
/// payload allocations of a worker's lifetime.
pub fn seed_payload_pool(recycle: &mut Producer<Box<BodyFramePayload>>) {
    for _ in 0..PAYLOAD_POOL_SIZE {
        // The ring is sized PAYLOAD_POOL_SIZE + 1, so seeding cannot fail;
        // dropping on the impossible error is still safe (just a smaller pool).
        let _ = recycle.push(Box::new(BodyFramePayload::new()));
    }
}

/// One processed body frame, published by the worker.
pub struct BodyFrame {
    /// Whether a person was tracked in this frame (detector hit while idle,
    /// landmark-confirmed while active).
    pub present: bool,
    /// Track confidence (see `BodyTrackingState::confidence`).
    pub confidence: f32,
    /// Content-normalized landmarks + visibility (unsmoothed; the main
    /// thread's One-Euro pass smooths at poll rate).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (unsmoothed).
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
    /// Worker-relative capture timestamp.
    pub timestamp: Duration,
    /// Mask + edges, when a pooled buffer was available and the full pipeline
    /// ran (absent for idle detector-only probes and under pool exhaustion).
    pub payload: Option<Box<BodyFramePayload>>,
}

/// A message from the body worker to the main thread.
// The Frame payload (66 landmark/world/velocity vectors) dwarfs Status;
// boxing it would add a per-frame heap allocation for a 64-entry ring, so the
// size asymmetry is the better trade (same call as the hand worker's msg).
#[allow(clippy::large_enum_variant)]
pub enum BodyWorkerMsg {
    /// One processed frame.
    Frame(BodyFrame),
    /// The inference backend label, sent once after the worker builds its
    /// sessions (models load on the worker thread — see the worker docs).
    Backend(&'static str),
    /// Lifecycle status change.
    Status(BodyTrackingStatus),
    /// Worker/pipeline counters for the most recent processed frame.
    Diagnostics(BodyWorkerDiagnostics),
    /// A pipeline/model error string (rare path — the allocation never
    /// touches the steady-state loop).
    Error(String),
    /// The negotiated camera format label, sent once when the source opens.
    CameraFormat(String),
}

/// Worker-side counters + pipeline diagnostics for one processed frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyWorkerDiagnostics {
    /// Pipeline-stage metrics for the latest frame.
    pub pipeline: PoseDiagnostics,
    /// Cumulative camera-frame drops (rate cap / idle throttle), distinct
    /// from ring backpressure below — same split as the hand worker.
    pub dropped_frames: u64,
    /// Cumulative result-ring backpressure drops (slow main-thread consumer).
    pub ring_full_drops: u64,
    /// Wall time acquiring + decoding the processed frame.
    pub capture_decode: Duration,
    /// Wall time since the previous processed frame (effective inference
    /// period).
    pub inference_interval: Duration,
    /// Cumulative pipeline (inference) errors.
    pub pipeline_errors: u64,
    /// Whether the idle throttle was requested for this frame.
    pub idle_throttled: bool,
}
```

Note: this module references `super::pipeline::PoseDiagnostics`, created in Task 9. To keep this task independently compilable, Task 8 initially declares the diagnostics field as a placeholder-free struct: **omit the `pipeline` field and its import in this task**; Task 9 adds both (the field, the import, and the corresponding line in the worker's diagnostics assembly). The Step 1 tests do not touch it.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::transport`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): ring transport types and recycled payload pool" -m "BodyWorkerMsg/BodyFrame plus the two-ring Box pool for the 256KB mask and edge list: three payloads seeded once, cycled worker-to-main-to-worker with pointer moves only; pool exhaustion degrades to payload-less frames instead of blocking or allocating." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 9: The two-stage pose pipeline (`pipeline.rs`)

Detector → ROI → landmark/mask, with detect-then-track, the idle detector-only probe, live tuning atomics, diagnostics, and init-allocated scratch. Image helpers are **adapted copies** of the hand pipeline's (per the plan brief: adapt, don't import — the hand module stays untouched and the two pipelines evolve independently); each copy is written out in full below.

**Files:**
- Create: `crates/wc-core/src/input/body/pipeline.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod pipeline;`), `crates/wc-core/src/input/body/transport.rs` (add the `pipeline: PoseDiagnostics` field deferred from Task 8)

**Interfaces:**
- Consumes: `crate::input::capture::Frame`, `crate::input::onnx::{ModelInference, InferenceError, Tensor}`, Tasks 4–8 items.
- Produces: `PosePipeline` (`new`, `set_live_tuning_source`, `diagnostics`, `process`), `PoseConfig`, `BodyLiveTuning`, `PoseResult`, `PoseDiagnostics`, `DetectorRunReason`, `#[cfg(test)] pub(crate) mod fixtures` — consumed by Tasks 11/12/14.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::input::capture::Frame;
    use crate::input::onnx::Tensor;

    /// Inference stub replaying fixed outputs.
    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl ModelInference for StaticInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    /// Inference stub that always fails — proves a stage was NOT invoked when
    /// a call would error the pipeline.
    struct FailingInference;

    impl ModelInference for FailingInference {
        fn run(&mut self, _input: &Tensor, _out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            Err(InferenceError::Run("must not run".into()))
        }
    }

    fn solid_frame() -> Frame {
        let mut f = Frame::default();
        f.fit_to(64, 48);
        f
    }

    fn person_pipeline() -> PosePipeline {
        PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: confident_landmark_outputs(),
            }),
            PoseConfig::default(),
        )
    }

    #[test]
    fn cold_start_detects_then_tracks() {
        let mut p = person_pipeline();
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        let frame = solid_frame();

        let r1 = p
            .process(&frame, false, Some(&mut payload))
            .expect("frame 1");
        assert!(r1.present);
        assert!(r1.confidence > 0.8);
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::ColdStart);
        // Landmarks land in content-norm [0, 1] with high visibility.
        for lm in &r1.landmarks {
            assert!(lm.pos.x.is_finite() && lm.pos.y.is_finite());
            assert!(lm.visibility > 0.7, "vis={}", lm.visibility);
        }
        // World landmarks decode from the [1, 117] tensor.
        assert!((r1.world_landmarks[0].x - 0.1).abs() < 1e-5);
        assert!((r1.world_landmarks[0].y - (-0.2)).abs() < 1e-5);

        // Frame 2: the carried aux-row track skips the detector entirely.
        let r2 = p.process(&frame, false, Some(&mut payload)).expect("frame 2");
        assert!(r2.present);
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::Tracking);
    }

    #[test]
    fn mask_and_edges_land_in_the_payload() {
        let mut p = person_pipeline();
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        p.process(&solid_frame(), false, Some(&mut payload))
            .expect("process");
        // The fixture's mask blob covers the crop centre; after warping, the
        // frame-space mask must be lit near the ROI centre and dark far away.
        let max = payload.mask.iter().copied().max().unwrap_or(0);
        assert!(max > 200, "mask never lit: max={max}");
        assert!(!payload.edges.is_empty(), "edges must be extracted");
        assert!(payload.edges.len() <= crate::input::body::MAX_EDGE_POINTS);
    }

    #[test]
    fn low_landmark_confidence_drops_the_track_and_fades_the_mask() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(StaticInference {
                outputs: low_confidence_landmark_outputs(),
            }),
            PoseConfig::default(),
        );
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        let r = p
            .process(&solid_frame(), false, Some(&mut payload))
            .expect("process");
        assert!(!r.present, "conf below threshold must read absent");
        // Next frame must re-detect (track not carried).
        p.process(&solid_frame(), false, Some(&mut payload))
            .expect("frame 2");
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::ColdStart);
    }

    #[test]
    fn empty_detector_output_reads_absent() {
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: empty_detector_outputs(),
            }),
            Box::new(FailingInference), // landmark stage must not run
            PoseConfig::default(),
        );
        let r = p.process(&solid_frame(), false, None).expect("process");
        assert!(!r.present);
        assert_eq!(r.confidence, 0.0);
    }

    #[test]
    fn detector_only_probe_skips_the_landmark_stage() {
        // Idle probe: hot detector + a landmark stage that would ERROR if
        // invoked. Present must still be reported (the wake path).
        let mut p = PosePipeline::new(
            Box::new(StaticInference {
                outputs: hot_person_detector_outputs(),
            }),
            Box::new(FailingInference),
            PoseConfig::default(),
        );
        let r = p.process(&solid_frame(), true, None).expect("probe");
        assert!(r.present, "idle probe must still report presence");
        assert!(r.confidence > 0.8);
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::IdleProbe);
    }

    #[test]
    fn invalid_frame_clears_the_track() {
        let mut p = person_pipeline();
        let good = solid_frame();
        p.process(&good, false, None).expect("acquire");
        let mut bad = Frame::default();
        bad.width = 10; // inconsistent: no bytes
        let r = p.process(&bad, false, None).expect("invalid frame is not an error");
        assert!(!r.present);
        assert_eq!(
            p.diagnostics().detector_reason,
            DetectorRunReason::InvalidFrame
        );
        p.process(&good, false, None).expect("reacquire");
        assert_eq!(p.diagnostics().detector_reason, DetectorRunReason::ColdStart);
    }

    #[test]
    fn live_tuning_updates_the_mask_alpha() {
        let tuning = std::sync::Arc::new(BodyLiveTuning::new(0.35));
        let mut p = person_pipeline();
        p.set_live_tuning_source(std::sync::Arc::clone(&tuning));
        tuning.set_mask_ema_alpha(0.9);
        assert!((tuning.mask_ema_alpha() - 0.9).abs() < 1e-6);
        // Round-trips through the atomic; the pipeline reads it per frame.
        let mut payload = crate::input::body::transport::BodyFramePayload::new();
        p.process(&solid_frame(), false, Some(&mut payload))
            .expect("process");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::pipeline`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod pipeline;` to `body/mod.rs`. In `transport.rs`, add to `BodyWorkerDiagnostics` (plus `use super::pipeline::PoseDiagnostics;`):

```rust
    /// Pipeline-stage metrics for the latest frame.
    pub pipeline: PoseDiagnostics,
```

Create `crates/wc-core/src/input/body/pipeline.rs`:

```rust
//! Two-stage BlazePose pipeline: a camera `Frame` in, landmarks + world
//! landmarks + a warped/EMA'd mask + silhouette edges out.
//!
//! Flow per frame: square-pad the frame; run the person detector ONLY when no
//! track is carried (detect-then-track — the aux landmark rows 33/34 supply
//! next frame's ROI, so a healthy track never pays the detector); warp the
//! rotated ROI into a 256² crop; run the landmark model; gate on its
//! pose-presence scalar; project the 39 rows back to square-norm; publish the
//! first 33 in content-norm (mask UV space); warp + EMA the segmentation
//! mask; extract silhouette edges into the pooled payload.
//!
//! The **idle detector-only probe** (`detector_only = true`) runs just the
//! detector as a presence sensor at the idle rate: landmarks/mask stages are
//! skipped, the carried track is cleared (stale after idle), and the mask
//! EMA decays so no stale silhouette lingers.
//!
//! All scratch (pad/resize/warp images, input/output tensors, decode buffer,
//! mask processor) is owned by the pipeline and refilled in place — the
//! steady-state frame path allocates nothing. Image helpers are adapted from
//! the validated hand pipeline (same conventions: `/255` RGB NHWC, square-pad
//! to the larger side, bilinear warp/resize with clamp-to-edge).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bevy::math::Vec3;
use image::RgbImage;

use super::detector::{
    best_person, decode_pose_detections_into, generate_pose_anchors, Anchor, PersonDetection,
    DETECTOR_INPUT, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN,
};
use super::edges::extract_edges;
use super::mask::{MaskProcessor, DEFAULT_MASK_EMA_ALPHA};
use super::roi::{
    project_body_landmarks, roi_from_body_landmarks, roi_from_detection, roi_trackable,
    ContentRect, RoiRect, LANDMARK_ROWS, LANDMARK_VALUES,
};
use super::transport::BodyFramePayload;
use super::{BodyLandmark, BODY_LANDMARK_COUNT, MASK_SIZE};
use crate::input::capture::Frame;
use crate::input::onnx::{InferenceError, ModelInference, Tensor};

/// Landmark model input side as `u32` (the warp target).
const LM_SIZE: u32 = 256;

/// IoU threshold for blending detections around the argmax seed
/// (MediaPipe's `min_suppression_threshold: 0.3`).
const PERSON_BLEND_IOU: f32 = 0.3;

/// Tunables for the pose pipeline.
#[derive(Debug, Clone)]
pub struct PoseConfig {
    /// Minimum detector score to accept a person (`min_score_thresh: 0.5`).
    pub detector_score_threshold: f32,
    /// Minimum pose-presence probability from the landmark model to keep the
    /// track (matches MediaPipe's default tracking confidence).
    pub presence_threshold: f32,
    /// Temporal EMA factor for the mask (see `mask::DEFAULT_MASK_EMA_ALPHA`);
    /// live-tunable through [`BodyLiveTuning`].
    pub mask_ema_alpha: f32,
}

impl Default for PoseConfig {
    fn default() -> Self {
        Self {
            detector_score_threshold: 0.5,
            presence_threshold: 0.5,
            mask_ema_alpha: DEFAULT_MASK_EMA_ALPHA,
        }
    }
}

/// Live (lock-free) tunables shared between the Bevy main thread and the
/// worker: the idle-throttle flag read by the worker *loop* and the mask EMA
/// factor read by this pipeline each frame. Same shape as the hand provider's
/// `MediaPipeLiveTuning` (f32 bit patterns in `AtomicU32`, all `Relaxed` —
/// independent scalars, one-frame-stale reads are harmless).
#[derive(Debug)]
pub struct BodyLiveTuning {
    /// Worker caps at the shared idle rate + detector-only probe while set.
    idle_throttle: AtomicBool,
    /// [`PoseConfig::mask_ema_alpha`] as `f32` bits.
    mask_ema_alpha: AtomicU32,
}

impl BodyLiveTuning {
    /// Build a tuning cell. The idle flag starts cleared (full rate).
    #[must_use]
    pub fn new(mask_ema_alpha: f32) -> Self {
        Self {
            idle_throttle: AtomicBool::new(false),
            mask_ema_alpha: AtomicU32::new(mask_ema_alpha.to_bits()),
        }
    }

    /// Live-set the idle-throttle flag (cheap Relaxed store; safe every frame).
    pub fn set_idle_throttle(&self, idle: bool) {
        self.idle_throttle.store(idle, Ordering::Relaxed);
    }

    /// Whether the idle detector-only throttle is requested.
    #[must_use]
    pub fn idle_throttle(&self) -> bool {
        self.idle_throttle.load(Ordering::Relaxed)
    }

    /// Live-set the mask EMA factor.
    pub fn set_mask_ema_alpha(&self, alpha: f32) {
        self.mask_ema_alpha.store(alpha.to_bits(), Ordering::Relaxed);
    }

    /// The current mask EMA factor.
    #[must_use]
    pub fn mask_ema_alpha(&self) -> f32 {
        f32::from_bits(self.mask_ema_alpha.load(Ordering::Relaxed))
    }
}

/// Why the detector ran or was skipped for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetectorRunReason {
    /// No carried track: the detector ran to (re)acquire.
    #[default]
    ColdStart,
    /// A carried track supplied the ROI; the detector was skipped.
    Tracking,
    /// Idle detector-only presence probe (landmark stage skipped).
    IdleProbe,
    /// The frame was invalid; no model stage ran.
    InvalidFrame,
}

impl DetectorRunReason {
    /// Static label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ColdStart => "cold_start",
            Self::Tracking => "tracking",
            Self::IdleProbe => "idle_probe",
            Self::InvalidFrame => "invalid_frame",
        }
    }
}

/// Timing and tracking metrics for the latest processed frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PoseDiagnostics {
    /// Total process time for one frame.
    pub total: Duration,
    /// Square-pad / preprocessing time.
    pub preprocess: Duration,
    /// Detector-stage time (zero when skipped).
    pub detector: Duration,
    /// Landmark/mask-stage time (zero when skipped).
    pub landmark: Duration,
    /// Why the detector ran or was skipped.
    pub detector_reason: DetectorRunReason,
    /// Whether a person was tracked this frame.
    pub present: bool,
    /// The frame's confidence (detector score or landmark presence).
    pub confidence: f32,
}

/// The published outcome of one processed frame.
pub struct PoseResult {
    /// Whether a person is tracked (idle probes report detector hits here).
    pub present: bool,
    /// Track confidence.
    pub confidence: f32,
    /// Content-normalized landmarks + visibility (all defaults when absent
    /// or in the idle probe).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Metric world landmarks (metres, hip-centred).
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT],
}

impl PoseResult {
    /// A no-person result.
    fn absent() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world_landmarks: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        }
    }
}

/// The two-stage pose pipeline: model sessions, anchors, carried track, mask
/// processor, and reused scratch buffers.
pub struct PosePipeline {
    detector: Box<dyn ModelInference>,
    landmark: Box<dyn ModelInference>,
    anchors: Vec<Anchor>,
    config: PoseConfig,
    /// Landmark-derived ROI carried to the next frame (detect-then-track).
    /// While present, `process` skips the detector; dropped when presence
    /// falls below threshold, the ROI leaves the content, the frame is
    /// unusable, or an idle probe runs.
    tracked: Option<RoiRect>,
    /// Optional live tuning shared with the provider systems.
    live_tuning: Option<Arc<BodyLiveTuning>>,
    /// Mask warp/EMA state (owns its 3×256 KB f32 buffers).
    mask: MaskProcessor,
    /// Diagnostics for the most recent processed frame.
    last_diagnostics: PoseDiagnostics,
    // --- reused scratch (see module docs; allocated once) ---
    square_buf: RgbImage,
    detector_resize_buf: RgbImage,
    warp_buf: RgbImage,
    detector_input: Tensor,
    landmark_input: Tensor,
    detector_outputs: Vec<Tensor>,
    landmark_outputs: Vec<Tensor>,
    detections: Vec<PersonDetection>,
}

impl PosePipeline {
    /// Build a pipeline from the two model stages.
    #[must_use]
    pub fn new(
        detector: Box<dyn ModelInference>,
        landmark: Box<dyn ModelInference>,
        config: PoseConfig,
    ) -> Self {
        Self {
            detector,
            landmark,
            anchors: generate_pose_anchors(),
            config,
            tracked: None,
            live_tuning: None,
            mask: MaskProcessor::new(),
            last_diagnostics: PoseDiagnostics::default(),
            square_buf: RgbImage::default(),
            detector_resize_buf: RgbImage::new(DETECTOR_INPUT, DETECTOR_INPUT),
            warp_buf: RgbImage::new(LM_SIZE, LM_SIZE),
            detector_input: Tensor {
                data: Vec::with_capacity(idx(DETECTOR_INPUT) * idx(DETECTOR_INPUT) * 3),
                shape: vec![1, idx(DETECTOR_INPUT), idx(DETECTOR_INPUT), 3],
            },
            landmark_input: Tensor {
                data: Vec::with_capacity(idx(LM_SIZE) * idx(LM_SIZE) * 3),
                shape: vec![1, idx(LM_SIZE), idx(LM_SIZE), 3],
            },
            detector_outputs: Vec::new(),
            landmark_outputs: Vec::new(),
            detections: Vec::new(),
        }
    }

    /// Attach the shared lock-free tuning cell.
    pub fn set_live_tuning_source(&mut self, source: Arc<BodyLiveTuning>) {
        self.live_tuning = Some(source);
    }

    /// Diagnostics for the most recent processed frame.
    #[must_use]
    pub fn diagnostics(&self) -> PoseDiagnostics {
        self.last_diagnostics
    }

    /// Run one frame. `detector_only` selects the idle presence probe (see
    /// module docs). `payload`, when given, receives the quantized mask and
    /// the extracted edges (full frames only; probes and absent frames decay
    /// the mask into it instead).
    ///
    /// # Errors
    /// Returns [`InferenceError`] if a model stage that was supposed to run
    /// fails. Invalid frames and empty detections are `Ok(absent)`, not
    /// errors.
    pub fn process(
        &mut self,
        frame: &Frame,
        detector_only: bool,
        mut payload: Option<&mut BodyFramePayload>,
    ) -> Result<PoseResult, InferenceError> {
        let frame_start = Instant::now();
        let mut diag = PoseDiagnostics::default();
        let alpha = self
            .live_tuning
            .as_ref()
            .map_or(self.config.mask_ema_alpha, |t| t.mask_ema_alpha());

        if !frame.is_consistent() || frame.width == 0 || frame.height == 0 {
            // A bad frame breaks tracking: re-acquire next frame.
            self.tracked = None;
            diag.detector_reason = DetectorRunReason::InvalidFrame;
            self.fade_mask_into(alpha, payload.as_deref_mut());
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult::absent());
        }
        let content = ContentRect::for_frame(frame.width, frame.height);

        // Square-pad into the reused buffer (taken out so stage methods can
        // borrow it beside &mut self; restored before every return).
        let stage = Instant::now();
        let square = {
            let mut square = std::mem::take(&mut self.square_buf);
            square_pad_into(frame, &mut square);
            square
        };
        diag.preprocess = stage.elapsed();

        if detector_only {
            // Idle probe: the detector is a presence sensor; a carried crop
            // track is stale after idle, so drop it.
            self.tracked = None;
            diag.detector_reason = DetectorRunReason::IdleProbe;
            let stage = Instant::now();
            let det = self.detect(&square);
            diag.detector = stage.elapsed();
            self.square_buf = square;
            let det = det?;
            let (present, confidence) =
                det.as_ref().map_or((false, 0.0), |d| (true, d.score));
            self.fade_mask_into(alpha, payload.as_deref_mut());
            diag.present = present;
            diag.confidence = confidence;
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult {
                present,
                confidence,
                ..PoseResult::absent()
            });
        }

        // Detect-then-track: run the detector only without a carried track.
        let roi = match self.tracked {
            Some(roi) => {
                diag.detector_reason = DetectorRunReason::Tracking;
                Some(roi)
            }
            None => {
                diag.detector_reason = DetectorRunReason::ColdStart;
                let stage = Instant::now();
                let det = self.detect(&square);
                diag.detector = stage.elapsed();
                match det {
                    Ok(d) => d.map(|d| roi_from_detection(&d)),
                    Err(e) => {
                        self.square_buf = square;
                        return Err(e);
                    }
                }
            }
        };
        let Some(roi) = roi else {
            // Nobody in frame: fade the mask, stay quiet.
            self.square_buf = square;
            self.fade_mask_into(alpha, payload.as_deref_mut());
            diag.total = frame_start.elapsed();
            self.last_diagnostics = diag;
            return Ok(PoseResult::absent());
        };

        let stage = Instant::now();
        let outcome = self.landmark_stage(&square, roi, content, alpha, payload.as_deref_mut());
        diag.landmark = stage.elapsed();
        self.square_buf = square;
        let outcome = outcome?;

        let result = match outcome {
            Some(tracked) => {
                // Carry the aux-row ROI only while it stays plausible.
                self.tracked = roi_trackable(&tracked.next_roi, content)
                    .then_some(tracked.next_roi);
                tracked.result
            }
            None => {
                // Presence collapsed: drop the track and fade the mask.
                self.tracked = None;
                self.fade_mask_into(alpha, payload.as_deref_mut());
                PoseResult::absent()
            }
        };
        diag.present = result.present;
        diag.confidence = result.confidence;
        diag.total = frame_start.elapsed();
        self.last_diagnostics = diag;
        Ok(result)
    }

    /// Detector stage: resize → NHWC tensor → run → decode → best person.
    fn detect(&mut self, square: &RgbImage) -> Result<Option<PersonDetection>, InferenceError> {
        resize_into(
            square,
            DETECTOR_INPUT,
            DETECTOR_INPUT,
            &mut self.detector_resize_buf,
        );
        fill_nhwc_unit(&self.detector_resize_buf, &mut self.detector_input);
        self.detector
            .run(&self.detector_input, &mut self.detector_outputs)?;
        let (boxes, scores) = pick_pose_detector_outputs(&self.detector_outputs)?;
        decode_pose_detections_into(
            boxes,
            scores,
            &self.anchors,
            self.config.detector_score_threshold,
            &mut self.detections,
        );
        Ok(best_person(&self.detections, PERSON_BLEND_IOU))
    }

    /// Landmark/mask stage for one ROI. `Ok(None)` = presence below
    /// threshold (person lost).
    fn landmark_stage(
        &mut self,
        square: &RgbImage,
        roi: RoiRect,
        content: ContentRect,
        alpha: f32,
        payload: Option<&mut BodyFramePayload>,
    ) -> Result<Option<TrackedBody>, InferenceError> {
        warp_roi_into(square, &roi, LM_SIZE, &mut self.warp_buf);
        fill_nhwc_unit(&self.warp_buf, &mut self.landmark_input);
        self.landmark
            .run(&self.landmark_input, &mut self.landmark_outputs)?;
        let picked = pick_pose_landmark_outputs(&self.landmark_outputs)?;
        if picked.confidence < self.config.presence_threshold {
            return Ok(None);
        }

        let rows = project_body_landmarks(picked.landmarks, &roi);
        let next_roi = roi_from_body_landmarks(&rows);

        // Publish the first 33 rows in content-norm (mask UV space).
        let mut landmarks = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for (dst, row) in landmarks.iter_mut().zip(rows.iter()) {
            dst.pos = content.to_content_norm(row.pos);
            dst.visibility = row.visibility;
        }
        let world_landmarks = decode_world_landmarks(picked.world);

        // Mask + edges into the pooled payload (worker-side, per spec).
        if let Some(payload) = payload {
            self.mask.ingest(picked.mask, &roi, content, alpha);
            self.mask.write_u8(&mut payload.mask);
            extract_edges(self.mask.smoothed(), &mut payload.edges);
        }

        Ok(Some(TrackedBody {
            result: PoseResult {
                present: true,
                confidence: picked.confidence,
                landmarks,
                world_landmarks,
            },
            next_roi,
        }))
    }

    /// Person-absent path: decay the mask EMA and, when a payload is
    /// supplied, publish the faded mask + its (shrinking) edge list so a
    /// stale silhouette never lingers on screen.
    fn fade_mask_into(&mut self, alpha: f32, payload: Option<&mut BodyFramePayload>) {
        self.mask.decay(alpha);
        if let Some(payload) = payload {
            self.mask.write_u8(&mut payload.mask);
            extract_edges(self.mask.smoothed(), &mut payload.edges);
        }
    }
}

/// One tracked frame's outcome: the published result plus the ROI to track
/// from next frame. Stack-only.
struct TrackedBody {
    result: PoseResult,
    next_roi: RoiRect,
}

// --- model output selection -----------------------------------------------

/// Select the detector outputs by shape: `[1, 2254, 12]` boxes and
/// `[1, 2254, 1]` scores.
fn pick_pose_detector_outputs(out: &[Tensor]) -> Result<(&[f32], &[f32]), InferenceError> {
    let boxes = out
        .iter()
        .find(|t| t.shape == [1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN])
        .ok_or_else(|| InferenceError::Run("pose detector: no [1,2254,12] output".into()))?;
    let scores = out
        .iter()
        .find(|t| t.shape == [1, POSE_ANCHOR_COUNT, 1])
        .ok_or_else(|| InferenceError::Run("pose detector: no [1,2254,1] output".into()))?;
    Ok((&boxes.data, &scores.data))
}

/// The landmark model's outputs the pipeline consumes.
struct PoseLandmarkOutputs<'a> {
    /// `[1, 195]`: 39 rows × (x, y, z, visibility, presence), crop pixels.
    landmarks: &'a [f32],
    /// Pose-presence probability (consumed raw — the sigmoid is baked into
    /// the graph; pinned against the vendored model in Task 14).
    confidence: f32,
    /// `[1, 256, 256, 1]` segmentation logits, crop space.
    mask: &'a [f32],
    /// `[1, 117]`: 39 × (x, y, z) metric world landmarks.
    world: &'a [f32],
}

/// Select the landmark model's outputs **by shape** (order-independent), so
/// extra outputs — e.g. the `[1, 64, 64, 39]` heatmap — are ignored wherever
/// they appear. The four shapes are mutually distinct, so shape matching is
/// unambiguous; a missing shape reports everything observed.
fn pick_pose_landmark_outputs(out: &[Tensor]) -> Result<PoseLandmarkOutputs<'_>, InferenceError> {
    let find = |shape: &[usize]| out.iter().find(|t| t.shape == shape);
    let (Some(landmarks), Some(conf), Some(mask), Some(world)) = (
        find(&[1, LANDMARK_ROWS * LANDMARK_VALUES]),
        find(&[1, 1]),
        find(&[1, MASK_SIZE, MASK_SIZE, 1]),
        find(&[1, LANDMARK_ROWS * 3]),
    ) else {
        let observed: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        return Err(InferenceError::Run(format!(
            "pose landmark: unexpected output shapes {observed:?}; \
             want [1,195], [1,1], [1,{MASK_SIZE},{MASK_SIZE},1], [1,117]"
        )));
    };
    let confidence = conf
        .data
        .first()
        .copied()
        .ok_or_else(|| InferenceError::Run("pose landmark: empty confidence".into()))?;
    Ok(PoseLandmarkOutputs {
        landmarks: &landmarks.data,
        confidence,
        mask: &mask.data,
        world: &world.data,
    })
}

/// Decode the `[1, 117]` world tensor: 39 × (x, y, z) metric metres,
/// hip-centred; the first [`BODY_LANDMARK_COUNT`] rows are published.
fn decode_world_landmarks(raw: &[f32]) -> [Vec3; BODY_LANDMARK_COUNT] {
    let mut out = [Vec3::ZERO; BODY_LANDMARK_COUNT];
    for (i, lm) in out.iter_mut().enumerate() {
        let base = i * 3;
        *lm = Vec3::new(
            raw.get(base).copied().unwrap_or(0.0),
            raw.get(base + 1).copied().unwrap_or(0.0),
            raw.get(base + 2).copied().unwrap_or(0.0),
        );
    }
    out
}

// --- image helpers (adapted from the validated hand pipeline) --------------

/// Square-pad a frame to its larger side (black bars), origin-centred, into a
/// reused buffer. (Re)allocates only when the side changes.
fn square_pad_into(frame: &Frame, out: &mut RgbImage) {
    let side = frame.width.max(frame.height);
    if out.width() != side || out.height() != side {
        *out = RgbImage::new(side, side);
    }
    let ox = (side - frame.width) / 2;
    let oy = (side - frame.height) / 2;
    let w = idx(frame.width);
    for y in 0..frame.height {
        let row = idx(y) * w * 3;
        for x in 0..frame.width {
            let i = row + idx(x) * 3;
            out.put_pixel(
                ox + x,
                oy + y,
                image::Rgb([frame.rgb[i], frame.rgb[i + 1], frame.rgb[i + 2]]),
            );
        }
    }
}

/// Bilinearly resize `src` into a reused `dst` (same half-pixel-centre
/// convention and downscale-aliasing tradeoff as the hand pipeline's
/// `resize_into` — MediaPipe's own preprocessing point-samples identically).
fn resize_into(src: &RgbImage, w: u32, h: u32, dst: &mut RgbImage) {
    if dst.width() != w || dst.height() != h {
        *dst = RgbImage::new(w, h);
    }
    if src.width() == 0 || src.height() == 0 || w == 0 || h == 0 {
        return;
    }
    let sx = dim(src.width()) / dim(w);
    let sy = dim(src.height()) / dim(h);
    for oy in 0..h {
        let y = (dim(oy) + 0.5) * sy - 0.5;
        for ox in 0..w {
            let x = (dim(ox) + 0.5) * sx - 0.5;
            dst.put_pixel(ox, oy, sample_bilinear_rgb(src, x, y));
        }
    }
}

/// Warp the rotated normalized ROI out of `square` into a reused `out_size`²
/// crop (bilinear, inverse-mapping each output pixel — mirrors
/// `project_body_landmarks`).
fn warp_roi_into(square: &RgbImage, roi: &RoiRect, out_size: u32, dst: &mut RgbImage) {
    if dst.width() != out_size || dst.height() != out_size {
        *dst = RgbImage::new(out_size, out_size);
    }
    let side = dim(square.width());
    let (sin, cos) = roi.rotation.sin_cos();
    let outf = dim(out_size);
    for oy in 0..out_size {
        for ox in 0..out_size {
            let u = (dim(ox) / outf - 0.5) * roi.size;
            let v = (dim(oy) / outf - 0.5) * roi.size;
            let nx = roi.cx + (u * cos - v * sin);
            let ny = roi.cy + (u * sin + v * cos);
            dst.put_pixel(ox, oy, sample_bilinear_rgb(square, nx * side, ny * side));
        }
    }
}

/// Fill `out` with the NHWC `[1, h, w, 3]` `f32` tensor (RGB in `[0, 1]`),
/// reusing its buffers (`clear()` keeps capacity).
fn fill_nhwc_unit(img: &RgbImage, out: &mut Tensor) {
    out.data.clear();
    for p in img.pixels() {
        out.data.push(f32::from(p[0]) / 255.0);
        out.data.push(f32::from(p[1]) / 255.0);
        out.data.push(f32::from(p[2]) / 255.0);
    }
    out.shape.clear();
    out.shape
        .extend_from_slice(&[1, idx(img.height()), idx(img.width()), 3]);
}

/// Clamped bilinear RGB sample (index-space coordinates, edge clamp).
fn sample_bilinear_rgb(img: &RgbImage, x: f32, y: f32) -> image::Rgb<u8> {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return image::Rgb([0, 0, 0]);
    }
    let xc = x.clamp(0.0, dim(w - 1));
    let yc = y.clamp(0.0, dim(h - 1));
    let fx = xc - xc.floor();
    let fy = yc - yc.floor();
    let x0 = floor_u32(xc);
    let y0 = floor_u32(yc);
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let mut out = [0_u8; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let p00 = f32::from(img.get_pixel(x0, y0)[c]);
        let p10 = f32::from(img.get_pixel(x1, y0)[c]);
        let p01 = f32::from(img.get_pixel(x0, y1)[c]);
        let p11 = f32::from(img.get_pixel(x1, y1)[c]);
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *slot = byte(top + (bot - top) * fy);
    }
    image::Rgb(out)
}

/// `u32` → `usize` (image index); infallible on all supported targets.
fn idx(v: u32) -> usize {
    usize::try_from(v).unwrap_or(0)
}

/// `u32` → `f32` for image dimensions (≤ 65535 for realistic frames).
fn dim(v: u32) -> f32 {
    f32::from(u16::try_from(v).unwrap_or(u16::MAX))
}

/// Floor a finite, non-negative, image-bounded float to a pixel index.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is finite, clamped >= 0, and bounded by the image dimension; \
              float->int has no From/TryFrom"
)]
fn floor_u32(v: f32) -> u32 {
    v.max(0.0).floor() as u32
}

/// Round a `[0, 255]`-clamped float to a colour byte.
#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255]; float->int has no From/TryFrom"
)]
fn byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

/// Test fixtures shared with the worker tests (Task 11): plausible mock
/// outputs for the detector and landmark stages.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::super::roi::{LANDMARK_ROWS, LANDMARK_VALUES};
    use super::{Tensor, MASK_SIZE, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN};

    /// Anchor index of the first anchor at stride-8 grid cell (14, 14): the
    /// image-centre-ish anchor the hot fixture lights up.
    pub(crate) const HOT_ANCHOR: usize = (14 * 28 + 14) * 2;

    /// Detector outputs with ONE confident person at the central anchor:
    /// box 0.3² centred there; keypoint 0 (mid-hip) at the anchor centre,
    /// keypoint 1 (scale point) 0.15 above it → ROI size 0.375, rotation 0.
    pub(crate) fn hot_person_detector_outputs() -> Vec<Tensor> {
        let mut boxes = vec![0.0_f32; POSE_ANCHOR_COUNT * POSE_REGRESSION_LEN];
        let base = HOT_ANCHOR * POSE_REGRESSION_LEN;
        boxes[base + 2] = 224.0 * 0.3; // w
        boxes[base + 3] = 224.0 * 0.3; // h
        boxes[base + 7] = -224.0 * 0.15; // kp1 y offset: 0.15 up
        let mut scores = vec![-100.0_f32; POSE_ANCHOR_COUNT];
        scores[HOT_ANCHOR] = 100.0;
        vec![
            Tensor {
                data: boxes,
                shape: vec![1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN],
            },
            Tensor {
                data: scores,
                shape: vec![1, POSE_ANCHOR_COUNT, 1],
            },
        ]
    }

    /// Detector outputs with every score pinned far below threshold.
    pub(crate) fn empty_detector_outputs() -> Vec<Tensor> {
        vec![
            Tensor::zeros(vec![1, POSE_ANCHOR_COUNT, POSE_REGRESSION_LEN]),
            Tensor {
                data: vec![-100.0; POSE_ANCHOR_COUNT],
                shape: vec![1, POSE_ANCHOR_COUNT, 1],
            },
        ]
    }

    /// Landmark outputs for a confident, well-spread pose: 39 rows spread
    /// down the crop (aux rows 33/34 form a valid upright tracking ROI), a
    /// centred mask blob, constant world rows, presence 0.9 — plus a
    /// heatmap-shaped extra output to prove shape-based picking skips it.
    pub(crate) fn confident_landmark_outputs() -> Vec<Tensor> {
        confident_landmark_outputs_with_conf(0.9)
    }

    /// As [`confident_landmark_outputs`] but with presence 0.1 (track lost).
    pub(crate) fn low_confidence_landmark_outputs() -> Vec<Tensor> {
        confident_landmark_outputs_with_conf(0.1)
    }

    fn confident_landmark_outputs_with_conf(conf: f32) -> Vec<Tensor> {
        let mut rows = vec![0.0_f32; LANDMARK_ROWS * LANDMARK_VALUES];
        for i in 0..LANDMARK_ROWS {
            let base = i * LANDMARK_VALUES;
            // x sweeps a little around centre; y walks down the crop.
            rows[base] = 118.0 + f32_from_usize(i % 5) * 5.0;
            rows[base + 1] = 50.0 + f32_from_usize(i) * 4.0;
            rows[base + 2] = 0.0;
            rows[base + 3] = 2.0; // visibility logit → ≈ 0.88
            rows[base + 4] = 2.0; // presence logit
        }
        // Aux tracking rows: centre (128, 128), scale point straight above at
        // (128, 96) → upright track ROI with size 2·(32/256)·roi_size·1.25.
        rows[33 * LANDMARK_VALUES] = 128.0;
        rows[33 * LANDMARK_VALUES + 1] = 128.0;
        rows[34 * LANDMARK_VALUES] = 128.0;
        rows[34 * LANDMARK_VALUES + 1] = 96.0;

        // Central mask blob: +8 logits in the middle quarter, −8 elsewhere.
        let mut mask = vec![-8.0_f32; MASK_SIZE * MASK_SIZE];
        for y in 96..160 {
            for x in 96..160 {
                mask[y * MASK_SIZE + x] = 8.0;
            }
        }

        // Constant world rows (metric): x 0.1, y −0.2, z 0.05.
        let mut world = vec![0.0_f32; LANDMARK_ROWS * 3];
        for i in 0..LANDMARK_ROWS {
            world[i * 3] = 0.1;
            world[i * 3 + 1] = -0.2;
            world[i * 3 + 2] = 0.05;
        }

        vec![
            // Deliberately shuffled order + an extra heatmap tensor: the
            // pipeline must pick by shape, not position.
            Tensor::zeros(vec![1, 64, 64, LANDMARK_ROWS]),
            Tensor {
                data: world,
                shape: vec![1, LANDMARK_ROWS * 3],
            },
            Tensor {
                data: vec![conf],
                shape: vec![1, 1],
            },
            Tensor {
                data: mask,
                shape: vec![1, MASK_SIZE, MASK_SIZE, 1],
            },
            Tensor {
                data: rows,
                shape: vec![1, LANDMARK_ROWS * LANDMARK_VALUES],
            },
        ]
    }

    /// Lossless small-usize → f32 for fixture math.
    fn f32_from_usize(v: usize) -> f32 {
        u16::try_from(v).map(f32::from).unwrap_or(0.0)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::pipeline input::body::transport`
Expected: PASS (7 pipeline tests + the 3 transport tests still green with the new diagnostics field).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): two-stage BlazePose pipeline with detect-then-track" -m "Detector runs only without a carried track (aux rows 33/34 supply the next ROI); landmark stage gates on the pose-presence scalar and publishes 33 content-norm landmarks plus metric world landmarks; mask warp/EMA and edge extraction fill the pooled payload; idle detector-only probe skips the landmark stage and decays the mask. Outputs picked by shape so heatmap extras are ignored. All scratch init-allocated." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 10: One-Euro body smoothing (`smoothing.rs`)

Poll-rate smoothing on the main thread, adapting the hand provider's `smoothing.rs` pattern (scalar One-Euro filters composed into `Vec3` filters, object-scale-normalized speed) for 33 fixed landmarks, plus the smoothed velocity derivation the pinned `BodyTrackingState.velocities` field requires.

**Files:**
- Create: `crates/wc-core/src/input/body/smoothing.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod smoothing;`)

**Interfaces:**
- Consumes: `BodyLandmark`, `BODY_LANDMARK_COUNT`, `bevy::math::Vec3`, `std::time::Duration`.
- Produces: `BodySmoother` (`new`, `clear`, `set_params`, `smooth`), `SmoothedBody`, `DEFAULT_MIN_CUTOFF`, `DEFAULT_BETA` — consumed by Task 12.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn body_at(x: f32) -> ([BodyLandmark; BODY_LANDMARK_COUNT], [Vec3; BODY_LANDMARK_COUNT]) {
        let mut lms = [BodyLandmark::default(); BODY_LANDMARK_COUNT];
        for (i, lm) in lms.iter_mut().enumerate() {
            // A spread body so object scale is well-defined.
            lm.pos = Vec3::new(x + f32_i(i) * 0.001, 0.2 + f32_i(i) * 0.015, 0.0);
            lm.visibility = 0.9;
        }
        let world = [Vec3::new(x, 0.0, 0.0); BODY_LANDMARK_COUNT];
        (lms, world)
    }

    fn f32_i(i: usize) -> f32 {
        u16::try_from(i).map(f32::from).unwrap_or(0.0)
    }

    #[test]
    fn first_frame_passes_through_without_lag() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (lms, world) = body_at(0.5);
        let out = s.smooth(&lms, &world, Duration::from_millis(0));
        assert!((out.landmarks[0].pos.x - 0.5).abs() < 1e-6);
        assert!((out.world[0].x - 0.5).abs() < 1e-6);
        assert_eq!(out.velocities[0], Vec3::ZERO, "no history → zero velocity");
        assert!((out.landmarks[0].visibility - 0.9).abs() < 1e-6, "visibility passes through");
    }

    #[test]
    fn eases_toward_a_moved_target_then_converges() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        let (b, wb) = body_at(0.5);
        s.smooth(&a, &wa, Duration::from_millis(0));
        let step = s.smooth(&b, &wb, Duration::from_millis(16));
        assert!(
            step.landmarks[0].pos.x > 0.0 && step.landmarks[0].pos.x < 0.5,
            "eased partway: {}",
            step.landmarks[0].pos.x
        );
        let mut last = step;
        for i in 2..240_u64 {
            last = s.smooth(&b, &wb, Duration::from_millis(i * 16));
        }
        assert!((last.landmarks[0].pos.x - 0.5).abs() < 0.01, "converged: {}", last.landmarks[0].pos.x);
    }

    #[test]
    fn velocity_tracks_motion_and_settles_to_zero() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        s.smooth(&a, &wa, Duration::from_millis(0));
        // Target jumps and holds: velocity spikes positive, then decays as
        // the smoothed position converges.
        let (b, wb) = body_at(0.4);
        let moving = s.smooth(&b, &wb, Duration::from_millis(16));
        assert!(moving.velocities[0].x > 0.0, "moving toward +x: {:?}", moving.velocities[0]);
        let mut settled = moving;
        for i in 2..300_u64 {
            settled = s.smooth(&b, &wb, Duration::from_millis(i * 16));
        }
        assert!(
            settled.velocities[0].length() < 0.05,
            "settled velocity ~0: {:?}",
            settled.velocities[0]
        );
    }

    #[test]
    fn clear_resets_to_cold_start() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        let (b, wb) = body_at(0.7);
        s.smooth(&a, &wa, Duration::from_millis(0));
        s.smooth(&b, &wb, Duration::from_millis(16));
        s.clear();
        // Cold start again: passthrough, zero velocity — a returning person
        // carries no stale momentum.
        let back = s.smooth(&b, &wb, Duration::from_millis(160));
        assert!((back.landmarks[0].pos.x - 0.7).abs() < 1e-5);
        assert_eq!(back.velocities[0], Vec3::ZERO);
    }

    #[test]
    fn set_params_retunes_without_resetting_state() {
        let mut s = BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA);
        let (a, wa) = body_at(0.0);
        s.smooth(&a, &wa, Duration::from_millis(0));
        s.smooth(&a, &wa, Duration::from_millis(16));
        // Near-zero cutoff, no adaptivity → very heavy smoothing; a big jump
        // barely moves. A reset would instead pass the target through.
        s.set_params(0.001, 0.0);
        let (b, wb) = body_at(1.0);
        let out = s.smooth(&b, &wb, Duration::from_millis(32));
        assert!(
            out.landmarks[0].pos.x < 0.1,
            "retuned heavy smoothing, not reset: {}",
            out.landmarks[0].pos.x
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::smoothing`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod smoothing;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/smoothing.rs`:

```rust
//! Poll-rate One-Euro smoothing for body landmarks (Casiez et al. 2012),
//! adapting the hand provider's pattern: the worker produces poses at the
//! inference cadence; the main thread eases the exposed pose toward the
//! latest result every frame so motion reads as fluid, with a speed-adaptive
//! cutoff normalized by the body's apparent size (distance-invariant
//! smoothing strength, following MediaPipe's LandmarksSmoothingCalculator).
//!
//! Velocities: the pinned `BodyTrackingState.velocities` are the finite
//! differences of the *smoothed* screen positions, additionally EMA'd
//! ([`VELOCITY_EMA_ALPHA`]) so Plan C's limb impulses don't flutter with
//! residual landmark noise.
//!
//! Filter banks are fixed arrays sized [`BODY_LANDMARK_COUNT`]; `clear()`
//! resets filter state in place — no allocation after construction.

use std::f32::consts::TAU;
use std::time::Duration;

use bevy::math::Vec3;

use super::{BodyLandmark, BODY_LANDMARK_COUNT};

/// Default minimum cutoff (Hz) — the at-rest smoothing strength. MediaPipe's
/// pose-landmark filtering default (`one_euro_filter { min_cutoff: 0.05 }`),
/// which is deliberately heavy: a still dancer must read as still. Live
/// tuning lands in Plan C's dev panel via [`BodySmoother::set_params`].
pub const DEFAULT_MIN_CUTOFF: f32 = 0.05;

/// Default speed coefficient (cutoff growth per body-scale/sec of speed) —
/// MediaPipe's pose default (`beta: 80`), so fast limbs cut through the
/// heavy at-rest smoothing with little lag.
pub const DEFAULT_BETA: f32 = 80.0;

/// Cutoff for the derivative low-pass (Hz) — the One-Euro paper's default.
const DERIVATE_CUTOFF: f32 = 1.0;

/// Floor for the apparent body size (normalized units), so a degenerate
/// collapsed landmark set never divides the speed by ~0.
const MIN_BODY_SCALE: f32 = 0.05;

/// EMA factor for the published velocities (fraction of the new finite
/// difference blended in per frame).
const VELOCITY_EMA_ALPHA: f32 = 0.5;

/// One-Euro smoothing factor for a cutoff frequency and timestep.
fn smoothing_alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (TAU * cutoff);
    1.0 / (1.0 + tau / dt)
}

/// Exponential low-pass: blend `x` toward `prev` by `alpha`.
fn low_pass(x: f32, alpha: f32, prev: f32) -> f32 {
    alpha * x + (1.0 - alpha) * prev
}

/// One-Euro filter for a single scalar channel.
struct OneEuroFilter {
    min_cutoff: f32,
    beta: f32,
    x_prev: Option<f32>,
    dx_prev: f32,
}

impl OneEuroFilter {
    const fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            x_prev: None,
            dx_prev: 0.0,
        }
    }

    /// Filter sample `x` over `dt` seconds; `value_scale` divides the speed
    /// driving the adaptive cutoff. First sample (or non-positive `dt`)
    /// passes through / holds.
    fn filter(&mut self, x: f32, dt: f32, value_scale: f32) -> f32 {
        let Some(x_prev) = self.x_prev else {
            self.x_prev = Some(x);
            return x;
        };
        if dt <= 0.0 {
            return x_prev;
        }
        let dx = (x - x_prev) / dt;
        let edx = low_pass(dx, smoothing_alpha(DERIVATE_CUTOFF, dt), self.dx_prev);
        self.dx_prev = edx;
        let cutoff = self.min_cutoff + self.beta * (edx * value_scale).abs();
        let x_hat = low_pass(x, smoothing_alpha(cutoff, dt), x_prev);
        self.x_prev = Some(x_hat);
        x_hat
    }

    /// Forget history (cold start) without touching parameters.
    fn reset(&mut self) {
        self.x_prev = None;
        self.dx_prev = 0.0;
    }

    /// Retune without disturbing filter state.
    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
    }
}

/// Three One-Euro filters, one per [`Vec3`] component.
struct Vec3Filter {
    c: [OneEuroFilter; 3],
}

impl Vec3Filter {
    const fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            c: [
                OneEuroFilter::new(min_cutoff, beta),
                OneEuroFilter::new(min_cutoff, beta),
                OneEuroFilter::new(min_cutoff, beta),
            ],
        }
    }

    fn filter(&mut self, v: Vec3, dt: f32, value_scale: f32) -> Vec3 {
        Vec3::new(
            self.c[0].filter(v.x, dt, value_scale),
            self.c[1].filter(v.y, dt, value_scale),
            self.c[2].filter(v.z, dt, value_scale),
        )
    }

    fn reset(&mut self) {
        for c in &mut self.c {
            c.reset();
        }
    }

    fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        for c in &mut self.c {
            c.set_params(min_cutoff, beta);
        }
    }
}

/// Apparent body size (normalized units): mean of the landmark bounding
/// box's width and height, floored at [`MIN_BODY_SCALE`]. Divides the speed
/// so smoothing strength is invariant to how close the dancer stands.
fn body_scale(landmarks: &[BodyLandmark; BODY_LANDMARK_COUNT]) -> f32 {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for lm in landmarks {
        min = min.min(lm.pos);
        max = max.max(lm.pos);
    }
    (((max.x - min.x) + (max.y - min.y)) * 0.5).max(MIN_BODY_SCALE)
}

/// One frame of smoothed output.
pub struct SmoothedBody {
    /// Smoothed content-norm landmarks (visibility passed through).
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    /// Smoothed metric world landmarks.
    pub world: [Vec3; BODY_LANDMARK_COUNT],
    /// EMA'd velocities of the smoothed screen positions (units/sec).
    pub velocities: [Vec3; BODY_LANDMARK_COUNT],
}

/// Eases the exposed body pose toward the latest inference result at poll
/// rate. One filter bank per landmark; [`Self::clear`] on person-loss so a
/// returning person starts fresh (no stale momentum).
pub struct BodySmoother {
    min_cutoff: f32,
    beta: f32,
    /// Monotonic time of the previous smooth; `None` until the first.
    last_now: Option<Duration>,
    pos: [Vec3Filter; BODY_LANDMARK_COUNT],
    world: [Vec3Filter; BODY_LANDMARK_COUNT],
    /// Previous smoothed positions (velocity finite differences).
    prev_pos: [Vec3; BODY_LANDMARK_COUNT],
    /// Whether `prev_pos` holds real history.
    has_prev: bool,
    /// EMA'd velocities.
    vel: [Vec3; BODY_LANDMARK_COUNT],
}

impl BodySmoother {
    /// Construct a smoother with the given One-Euro parameters.
    #[must_use]
    pub fn new(min_cutoff: f32, beta: f32) -> Self {
        Self {
            min_cutoff,
            beta,
            last_now: None,
            pos: std::array::from_fn(|_| Vec3Filter::new(min_cutoff, beta)),
            world: std::array::from_fn(|_| Vec3Filter::new(min_cutoff, beta)),
            prev_pos: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            has_prev: false,
            vel: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        }
    }

    /// Forget all state (person left / worker restart). The next
    /// [`Self::smooth`] is a cold start: passthrough, zero velocity. Resets
    /// in place — no allocation.
    pub fn clear(&mut self) {
        self.last_now = None;
        self.has_prev = false;
        self.vel = [Vec3::ZERO; BODY_LANDMARK_COUNT];
        for f in &mut self.pos {
            f.reset();
        }
        for f in &mut self.world {
            f.reset();
        }
    }

    /// Live-retune every channel without resetting filter state.
    pub fn set_params(&mut self, min_cutoff: f32, beta: f32) {
        self.min_cutoff = min_cutoff;
        self.beta = beta;
        for f in &mut self.pos {
            f.set_params(min_cutoff, beta);
        }
        for f in &mut self.world {
            f.set_params(min_cutoff, beta);
        }
    }

    /// Advance smoothing to `now`, easing toward the target arrays (the
    /// latest worker result, held constant between inference frames), and
    /// return the smoothed pose + velocities.
    pub fn smooth(
        &mut self,
        target: &[BodyLandmark; BODY_LANDMARK_COUNT],
        target_world: &[Vec3; BODY_LANDMARK_COUNT],
        now: Duration,
    ) -> SmoothedBody {
        let dt = self
            .last_now
            .map_or(0.0, |prev| now.saturating_sub(prev).as_secs_f32());
        self.last_now = Some(now);
        // Screen positions normalize speed by apparent body size; metric
        // world positions use unit scale.
        let pos_scale = 1.0 / body_scale(target);

        let mut out = SmoothedBody {
            landmarks: *target,
            world: *target_world,
            velocities: [Vec3::ZERO; BODY_LANDMARK_COUNT],
        };
        for i in 0..BODY_LANDMARK_COUNT {
            out.landmarks[i].pos = self.pos[i].filter(target[i].pos, dt, pos_scale);
            out.world[i] = self.world[i].filter(target_world[i], dt, 1.0);
            // Velocity: finite-difference the SMOOTHED position, then EMA.
            let v_raw = if self.has_prev && dt > 0.0 {
                (out.landmarks[i].pos - self.prev_pos[i]) / dt
            } else {
                Vec3::ZERO
            };
            self.vel[i] += (v_raw - self.vel[i]) * VELOCITY_EMA_ALPHA;
            out.velocities[i] = self.vel[i];
            self.prev_pos[i] = out.landmarks[i].pos;
        }
        self.has_prev = true;
        out
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::smoothing`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): One-Euro landmark smoothing with EMA velocities" -m "Poll-rate smoothing adapting the hand provider's pattern for 33 fixed landmarks: body-scale-normalized adaptive cutoff (MediaPipe pose defaults 0.05/80), in-place clear for returning-person cold starts, and finite-difference velocities EMA'd for Plan C's limb impulses." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 11: The body worker thread (`worker.rs`)

The capture→process→publish loop, adapted from the hand worker: budget decision before capture (over-budget frames drained undecoded), idle throttle re-read every iteration with edge-triggered hardware capture throttle, ring backpressure counted separately from camera drops — plus the body-specific pieces: models built ON the worker thread via a `PipelineFactory`, and payload claiming from the recycle ring.

**Files:**
- Create: `crates/wc-core/src/input/body/worker.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod worker;`)

**Interfaces:**
- Consumes: `capture::{Frame, FrameSource, CaptureError, IDLE_INFERENCE_HZ}`, `onnx::ort::OrtInference`, Tasks 8/9 items.
- Produces: `SourceFactory`, `PipelineFactory`, `WorkerHandle`, `spawn_body_worker`, `load_pose_pipeline`, `POSE_DETECTION_MODEL`, `POSE_LANDMARK_MODEL` — consumed by Tasks 12/15.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use std::time::Instant;

    use super::super::pipeline::fixtures::{
        confident_landmark_outputs, empty_detector_outputs, hot_person_detector_outputs,
    };
    use super::super::pipeline::{PoseConfig, PosePipeline};
    use super::super::transport::{seed_payload_pool, PAYLOAD_POOL_SIZE};
    use super::*;
    use crate::input::capture::MockFrameSource;
    use crate::input::onnx::{InferenceError, ModelInference, Tensor};

    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl ModelInference for StaticInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    struct FailingInference;

    impl ModelInference for FailingInference {
        fn run(&mut self, _input: &Tensor, _out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            Err(InferenceError::Run("boom".into()))
        }
    }

    fn looping_solid_source() -> SourceFactory {
        Box::new(|| {
            let mut f = crate::input::capture::Frame::default();
            f.fit_to(64, 48);
            let src: Box<dyn crate::input::capture::FrameSource> =
                Box::new(MockFrameSource::looping(vec![f]));
            Ok(src)
        })
    }

    fn person_pipeline_factory() -> PipelineFactory {
        Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: hot_person_detector_outputs(),
                    }),
                    Box::new(StaticInference {
                        outputs: confident_landmark_outputs(),
                    }),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        })
    }

    fn empty_pipeline_factory() -> PipelineFactory {
        Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: empty_detector_outputs(),
                    }),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        })
    }

    /// Build the rings + tuning a worker needs; returns everything the test
    /// drives.
    fn harness(
        idle: bool,
    ) -> (
        std::sync::Arc<super::super::pipeline::BodyLiveTuning>,
        rtrb::Producer<Box<super::super::transport::BodyFramePayload>>,
        rtrb::Consumer<super::super::transport::BodyWorkerMsg>,
        rtrb::Producer<super::super::transport::BodyWorkerMsg>,
        rtrb::Consumer<Box<super::super::transport::BodyFramePayload>>,
    ) {
        let tuning = std::sync::Arc::new(super::super::pipeline::BodyLiveTuning::new(0.35));
        tuning.set_idle_throttle(idle);
        let (mut recycle_tx, recycle_rx) =
            rtrb::RingBuffer::new(PAYLOAD_POOL_SIZE + 1);
        seed_payload_pool(&mut recycle_tx);
        let (result_tx, result_rx) = rtrb::RingBuffer::new(64);
        (tuning, recycle_tx, result_rx, result_tx, recycle_rx)
    }

    /// Drain messages until `deadline`, recycling payloads and tallying.
    struct Tally {
        frames: u64,
        person_frames: u64,
        payload_frames: u64,
        backend: Option<&'static str>,
        statuses: Vec<super::super::BodyTrackingStatus>,
        errors: u64,
        max_dropped: u64,
        mask_ptrs: std::collections::HashSet<*const u8>,
    }

    fn drain(
        consumer: &mut rtrb::Consumer<super::super::transport::BodyWorkerMsg>,
        recycle: &mut rtrb::Producer<Box<super::super::transport::BodyFramePayload>>,
        deadline: Instant,
    ) -> Tally {
        use super::super::transport::BodyWorkerMsg;
        let mut t = Tally {
            frames: 0,
            person_frames: 0,
            payload_frames: 0,
            backend: None,
            statuses: Vec::new(),
            errors: 0,
            max_dropped: 0,
            mask_ptrs: std::collections::HashSet::new(),
        };
        while Instant::now() < deadline {
            while let Ok(msg) = consumer.pop() {
                match msg {
                    BodyWorkerMsg::Frame(mut f) => {
                        t.frames += 1;
                        if f.present {
                            t.person_frames += 1;
                        }
                        if let Some(payload) = f.payload.take() {
                            t.payload_frames += 1;
                            t.mask_ptrs.insert(payload.mask.as_ptr());
                            let _ = recycle.push(payload);
                        }
                    }
                    BodyWorkerMsg::Backend(b) => t.backend = Some(b),
                    BodyWorkerMsg::Status(s) => t.statuses.push(s),
                    BodyWorkerMsg::Diagnostics(d) => {
                        t.max_dropped = t.max_dropped.max(d.dropped_frames);
                    }
                    BodyWorkerMsg::Error(_) => t.errors += 1,
                    BodyWorkerMsg::CameraFormat(_) => {}
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        t
    }

    #[test]
    fn worker_streams_person_frames_with_recycled_payloads() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            person_pipeline_factory(),
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(600),
        );
        handle.stop();
        assert!(t.person_frames >= 3, "person frames: {}", t.person_frames);
        assert!(t.payload_frames >= 3, "payload frames: {}", t.payload_frames);
        assert_eq!(t.backend, Some("mock/backend"));
        assert!(
            t.statuses.contains(&super::super::BodyTrackingStatus::Streaming),
            "streaming status never reported: {:?}",
            t.statuses
        );
        assert!(
            t.mask_ptrs.len() <= PAYLOAD_POOL_SIZE,
            "steady state must reuse the pooled buffers, saw {} distinct",
            t.mask_ptrs.len()
        );
    }

    #[test]
    fn worker_honors_max_hz_by_dropping_over_budget_frames() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            empty_pipeline_factory(),
            1,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(120),
        );
        handle.stop();
        assert!(t.frames <= 1, "1 Hz cap processed {} frames in 120 ms", t.frames);
        assert!(t.max_dropped > 0, "over-budget frames were not reported dropped");
    }

    #[test]
    fn idle_probe_still_emits_person_bearing_frames() {
        // Wake contract: the idle throttle runs detector-only, and a person
        // seen by the detector must still cross the ring so presence can
        // reset the idle timer. The landmark stage is a FailingInference —
        // if the probe ever invoked it, frames would turn into errors.
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(true);
        let factory: PipelineFactory = Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(StaticInference {
                        outputs: hot_person_detector_outputs(),
                    }),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        });
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(600),
        );
        handle.stop();
        assert!(t.person_frames >= 1, "idle probe never emitted presence");
        assert_eq!(t.errors, 0, "landmark stage must not run while idle");
        assert_eq!(t.payload_frames, 0, "idle probes must not claim payloads");
    }

    #[test]
    fn pipeline_factory_failure_reports_failed_status() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let factory: PipelineFactory = Box::new(|| Err("no models".into()));
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(200),
        );
        handle.stop();
        assert!(
            t.statuses.contains(&super::super::BodyTrackingStatus::Failed),
            "factory failure must surface as Failed: {:?}",
            t.statuses
        );
        assert!(t.errors >= 1, "the error string must cross the ring");
    }

    #[test]
    fn pipeline_errors_are_counted_and_surfaced() {
        let (tuning, mut recycle_tx, mut result_rx, result_tx, recycle_rx) = harness(false);
        let factory: PipelineFactory = Box::new(|| {
            Ok((
                PosePipeline::new(
                    Box::new(FailingInference),
                    Box::new(FailingInference),
                    PoseConfig::default(),
                ),
                "mock/backend",
            ))
        });
        let mut handle = spawn_body_worker(
            looping_solid_source(),
            factory,
            30,
            tuning,
            result_tx,
            recycle_rx,
        );
        let t = drain(
            &mut result_rx,
            &mut recycle_tx,
            Instant::now() + std::time::Duration::from_millis(200),
        );
        handle.stop();
        assert!(t.errors >= 1, "pipeline error not surfaced");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::worker`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod worker;` to `body/mod.rs`, then create `crates/wc-core/src/input/body/worker.rs`:

```rust
//! Background worker running the [`super::pipeline::PosePipeline`] off the
//! Bevy main thread, publishing results over a lock-free `rtrb` ring.
//!
//! Adapted from the hand-tracking worker (same rate-cap mechanism: the
//! budget decision happens BEFORE capture, and over-budget frames are
//! drained **undecoded** so the camera stream stays fresh — newest frame
//! wins — while the decode cost is skipped). Body-specific differences:
//!
//! - **Models are built on this thread** via the [`PipelineFactory`]: body
//!   tracking starts on sketch entry, and a first-launch CoreML compile must
//!   not hitch the render thread. The backend label crosses back as
//!   [`BodyWorkerMsg::Backend`].
//! - **Payload pool client:** full frames claim a pooled
//!   [`super::transport::BodyFramePayload`] from the recycle ring; idle
//!   probes never touch the pool; pool exhaustion degrades to payload-less
//!   frames (landmarks stay fresh, the mask skips a frame) instead of
//!   blocking or allocating.
//! - The idle throttle selects the pipeline's detector-only probe in
//!   addition to lowering the rate to the shared
//!   `capture::IDLE_INFERENCE_HZ`; the hardware capture throttle is
//!   dispatched edge-triggered exactly like the hand worker.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};

use super::pipeline::{BodyLiveTuning, PoseConfig, PosePipeline};
use super::transport::{BodyFrame, BodyFramePayload, BodyWorkerDiagnostics, BodyWorkerMsg};
use super::BodyTrackingStatus;
use crate::input::capture::{CaptureError, Frame, FrameSource, IDLE_INFERENCE_HZ};
use crate::input::onnx::ModelInference;

/// Vendored detector filename under the model directory.
pub const POSE_DETECTION_MODEL: &str = "pose_detection.onnx";

/// Vendored landmark/segmentation filename under the model directory.
pub const POSE_LANDMARK_MODEL: &str = "pose_landmark_full.onnx";

/// Idle backoff when the source has no frame ready, so a non-blocking source
/// can't busy-spin a core (mainly guards mock sources).
const IDLE_POLL: Duration = Duration::from_millis(2);

/// Creates the frame source on the worker thread (deferred so `!Send` camera
/// backends are built where they are used; the factory itself is `Send`).
pub type SourceFactory = Box<dyn FnOnce() -> Result<Box<dyn FrameSource>, CaptureError> + Send>;

/// Builds the pose pipeline (model files + ort sessions) on the worker
/// thread, returning it with the combined inference backend label. The
/// error string is what crosses the ring as [`BodyWorkerMsg::Error`].
pub type PipelineFactory =
    Box<dyn FnOnce() -> Result<(PosePipeline, &'static str), String> + Send>;

/// Handle to a running worker; dropping or [`Self::stop`] joins the thread.
pub struct WorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Signal the worker to stop and join it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Load the two vendored pose models and build the pipeline (worker-thread
/// only — the CoreML compile can take seconds on first launch). Returns the
/// pipeline plus the combined backend label for diagnostics.
///
/// # Errors
/// Returns a human-readable string when a model file is unreadable or a
/// session fails to build.
pub fn load_pose_pipeline(model_dir: &Path) -> Result<(PosePipeline, &'static str), String> {
    let (detector, det_backend) = load_model(model_dir, POSE_DETECTION_MODEL)?;
    let (landmark, lm_backend) = load_model(model_dir, POSE_LANDMARK_MODEL)?;
    let backend = combined_backend(det_backend, lm_backend);
    Ok((
        PosePipeline::new(detector, landmark, PoseConfig::default()),
        backend,
    ))
}

/// Load one ONNX model as a boxed [`ModelInference`] with its backend label.
fn load_model(
    dir: &Path,
    name: &str,
) -> Result<(Box<dyn ModelInference>, &'static str), String> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("read model {}: {e}", path.display()))?;
    let model = crate::input::onnx::ort::OrtInference::load(&bytes).map_err(|e| e.to_string())?;
    let backend = model.backend();
    let boxed: Box<dyn ModelInference> = Box::new(model);
    Ok((boxed, backend))
}

/// Combine the two stages' backend labels (they normally agree; a mixed
/// state must not hide the slow path — same policy as the hand provider).
fn combined_backend(detector: &'static str, landmark: &'static str) -> &'static str {
    if detector == landmark {
        detector
    } else {
        "ort/mixed"
    }
}

/// Spawn the worker thread. Runs until [`WorkerHandle::stop`] (or drop):
/// builds the pipeline (via `make_pipeline`) and the camera (via
/// `make_source`) on the worker thread, then captures + processes at up to
/// `max_hz` (or the idle cap while `tuning.idle_throttle()` holds), pushing
/// [`BodyWorkerMsg`]s to `producer` and claiming mask payloads from
/// `recycle`. OS thread-spawn failure is reported through the ring rather
/// than swallowed (same producer-slot reclaim as the hand worker).
#[must_use]
pub fn spawn_body_worker(
    make_source: SourceFactory,
    make_pipeline: PipelineFactory,
    max_hz: u32,
    tuning: Arc<BodyLiveTuning>,
    producer: Producer<BodyWorkerMsg>,
    recycle: Consumer<Box<BodyFramePayload>>,
) -> WorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let min_inference_interval = inference_interval(max_hz);

    // `producer` must move into the closure, but a failed thread spawn drops
    // the closure without handing it back; the shared slot lets the failure
    // branch reclaim it and still report the error (hand-worker pattern).
    let producer_slot = Arc::new(Mutex::new(Some(producer)));
    let producer_for_thread = Arc::clone(&producer_slot);

    let spawn_result = std::thread::Builder::new()
        .name("wc-body-worker".into())
        .spawn(move || {
            let Some(mut producer) = producer_for_thread
                .lock()
                .ok()
                .and_then(|mut slot| slot.take())
            else {
                // Unreachable in practice (see the hand worker's rationale);
                // guarded so a refactor can't turn it into a thread panic.
                return;
            };
            // Build the models/sessions HERE so CoreML compiles off the main
            // thread; failure is a Failed status + the error string.
            let mut pipeline = match make_pipeline() {
                Ok((pipeline, backend)) => {
                    let _ = producer.push(BodyWorkerMsg::Backend(backend));
                    pipeline
                }
                Err(e) => {
                    tracing::error!("body worker: pipeline build failed: {e}");
                    let _ = producer.push(BodyWorkerMsg::Error(e));
                    let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::Failed));
                    return;
                }
            };
            pipeline.set_live_tuning_source(Arc::clone(&tuning));
            // Build the source on this thread (!Send backends are fine).
            let source = match make_source() {
                Ok(source) => source,
                Err(e) => {
                    tracing::error!("body worker: camera open failed: {e}");
                    let _ = producer.push(BodyWorkerMsg::Error(e.to_string()));
                    let _ = producer
                        .push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                    return;
                }
            };
            run_worker_loop(
                &stop_thread,
                source,
                pipeline,
                min_inference_interval,
                &tuning,
                producer,
                recycle,
            );
        });

    let join = match spawn_result {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!("failed to spawn body worker thread: {e}");
            if let Ok(mut slot) = producer_slot.lock() {
                if let Some(mut producer) = slot.take() {
                    let _ = producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::Failed));
                }
            }
            None
        }
    };

    WorkerHandle { stop, join }
}

/// Cumulative drop counters, split by cause (camera rate-cap drops vs ring
/// backpressure — the same must-not-fold split as the hand worker).
#[derive(Debug, Default)]
struct DropCounters {
    camera: u64,
    ring_full: u64,
}

/// The capture→process→publish loop (worker thread until `stop`).
#[allow(clippy::too_many_arguments, reason = "worker wiring, called once")]
fn run_worker_loop(
    stop: &AtomicBool,
    mut source: Box<dyn FrameSource>,
    mut pipeline: PosePipeline,
    min_inference_interval: Option<Duration>,
    tuning: &BodyLiveTuning,
    mut producer: Producer<BodyWorkerMsg>,
    mut recycle: Consumer<Box<BodyFramePayload>>,
) {
    let start = Instant::now();
    let mut frame = Frame::default();
    let mut last_inference: Option<Instant> = None;
    let mut drops = DropCounters::default();
    let mut pipeline_errors = 0_u64;
    // The payload currently held by the worker (claimed from the pool,
    // handed off inside a Frame message on success, retained on error).
    let mut spare: Option<Box<BodyFramePayload>> = None;
    let idle_inference_interval = idle_capped_interval(min_inference_interval);
    // Edge-triggered hardware capture throttle (see the hand worker).
    let mut last_throttle: Option<bool> = None;

    if let Some(label) = source.format_label().map(str::to_owned) {
        push_msg(&mut producer, BodyWorkerMsg::CameraFormat(label), &mut drops);
    }
    push_msg(
        &mut producer,
        BodyWorkerMsg::Status(BodyTrackingStatus::Streaming),
        &mut drops,
    );

    while !stop.load(Ordering::Relaxed) {
        let loop_start = Instant::now();
        // Re-read the idle flag every iteration (Relaxed; one-iteration
        // staleness is harmless).
        let idle_throttled = tuning.idle_throttle();
        if last_throttle != Some(idle_throttled) {
            source.set_capture_throttle(idle_throttled);
            last_throttle = Some(idle_throttled);
        }
        let min_interval = if idle_throttled {
            idle_inference_interval
        } else {
            min_inference_interval
        };
        // Budget decision BEFORE capture: over-budget frames drain undecoded
        // (newest frame wins, decode cost skipped — the throttle's thermal win).
        if !should_process_frame(last_inference, loop_start, min_interval) {
            match source.discard_frame() {
                Ok(true) => {
                    drops.camera = drops.camera.saturating_add(1);
                }
                Ok(false) => {}
                Err(_) => {
                    let _ = producer
                        .push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                }
            }
            std::thread::sleep(IDLE_POLL);
            continue;
        }

        match source.next_frame(&mut frame) {
            Ok(true) => {
                let capture_decode = loop_start.elapsed();
                let now = loop_start.duration_since(start);
                let dt = last_inference
                    .map_or(Duration::ZERO, |last| loop_start.duration_since(last));
                last_inference = Some(loop_start);
                // Full frames claim a pooled payload; idle probes never do.
                if !idle_throttled && spare.is_none() {
                    spare = recycle.pop().ok();
                }
                let payload_ref = if idle_throttled {
                    None
                } else {
                    spare.as_deref_mut()
                };
                match pipeline.process(&frame, idle_throttled, payload_ref) {
                    Ok(result) => {
                        let payload = if idle_throttled { None } else { spare.take() };
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, BodyWorkerMsg::Diagnostics(diag), &mut drops);
                        push_msg(
                            &mut producer,
                            BodyWorkerMsg::Frame(BodyFrame {
                                present: result.present,
                                confidence: result.confidence,
                                landmarks: result.landmarks,
                                world_landmarks: result.world_landmarks,
                                timestamp: now,
                                payload,
                            }),
                            &mut drops,
                        );
                    }
                    Err(e) => {
                        // Count + forward (rare path; the spare payload is
                        // retained for the next frame).
                        pipeline_errors = pipeline_errors.saturating_add(1);
                        push_msg(
                            &mut producer,
                            BodyWorkerMsg::Error(e.to_string()),
                            &mut drops,
                        );
                        let diag = worker_diag(
                            &pipeline,
                            &drops,
                            capture_decode,
                            dt,
                            pipeline_errors,
                            idle_throttled,
                        );
                        push_msg(&mut producer, BodyWorkerMsg::Diagnostics(diag), &mut drops);
                    }
                }
            }
            Ok(false) => {
                std::thread::sleep(IDLE_POLL);
            }
            Err(_) => {
                let _ =
                    producer.push(BodyWorkerMsg::Status(BodyTrackingStatus::CameraUnavailable));
                std::thread::sleep(IDLE_POLL);
            }
        }
    }
}

/// Minimum interval between inference runs for a requested max rate.
fn inference_interval(max_hz: u32) -> Option<Duration> {
    (max_hz > 0).then(|| Duration::from_secs_f64(1.0 / f64::from(max_hz)))
}

/// Minimum interval while the idle throttle is engaged: `max(active, idle)` —
/// the idle cap may only ever *slow* inference (hand-worker semantics).
fn idle_capped_interval(active: Option<Duration>) -> Option<Duration> {
    inference_interval(IDLE_INFERENCE_HZ).map(|idle| active.map_or(idle, |a| a.max(idle)))
}

/// Whether a fresh frame is allowed to run inference now.
fn should_process_frame(
    last_inference: Option<Instant>,
    now: Instant,
    min_interval: Option<Duration>,
) -> bool {
    match (last_inference, min_interval) {
        (_, None) | (None, Some(_)) => true,
        (Some(last), Some(interval)) => now.duration_since(last) >= interval,
    }
}

/// Assemble a diagnostics snapshot (shared by success and error paths).
fn worker_diag(
    pipeline: &PosePipeline,
    drops: &DropCounters,
    capture_decode: Duration,
    inference_interval: Duration,
    pipeline_errors: u64,
    idle_throttled: bool,
) -> BodyWorkerDiagnostics {
    BodyWorkerDiagnostics {
        pipeline: pipeline.diagnostics(),
        dropped_frames: drops.camera,
        ring_full_drops: drops.ring_full,
        capture_decode,
        inference_interval,
        pipeline_errors,
        idle_throttled,
    }
}

/// Push a message, counting a ring-full failure as backpressure (never as a
/// camera drop). Never blocks the worker.
fn push_msg(producer: &mut Producer<BodyWorkerMsg>, msg: BodyWorkerMsg, drops: &mut DropCounters) {
    if producer.push(msg).is_err() {
        drops.ring_full = drops.ring_full.saturating_add(1);
    }
}

```

(The test module imports `InferenceError` itself for its stubs; the
production code above deliberately does not.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::worker`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): body worker thread with on-thread model loading and payload pool" -m "Adapts the hand worker's budget-before-capture rate cap, idle throttle with edge-triggered hardware capture throttle, and split drop counters; builds the ort sessions on the worker thread (backend label crosses the ring) and claims mask payloads from the recycle ring with graceful pool-exhaustion degradation." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 12: Main-thread systems and final plugin wiring (`systems.rs`)

Request-driven start/stop, ring drain, One-Euro smoothing, state/mask/edges publication, payload recycling, the presence→`InteractionTimer` hook (design decision 1), and the finished `BodyTrackingPlugin::build` with its data-flow doc.

**Files:**
- Create: `crates/wc-core/src/input/body/systems.rs`
- Modify: `crates/wc-core/src/input/body/mod.rs` (add `pub mod systems;`, finalize the plugin)

**Interfaces:**
- Consumes: everything from Tasks 3–11; `crate::lifecycle::idle::InteractionTimer`; `bevy::input::InputSystems`.
- Produces: `BodyTrackingWorker` (resource), `init_mask_texture`, `sync_body_tracking`, `poll_body_worker`, `open_camera_source` — plus the final plugin.

- [ ] **Step 1: Write the failing test**

App-level tests at the footer of `systems.rs`:

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use std::time::Duration;

    use bevy::asset::AssetPlugin;
    use bevy::prelude::*;

    use super::super::pipeline::fixtures::{
        confident_landmark_outputs, empty_detector_outputs, hot_person_detector_outputs,
    };
    use super::super::pipeline::{PoseConfig, PosePipeline};
    use super::super::{
        BodyTrackingDiagnostics, BodyTrackingPlugin, BodyTrackingRequest, BodyTrackingState,
        MaskTexture, SilhouetteEdges,
    };
    use super::*;
    use crate::input::capture::{Frame, MockFrameSource};
    use crate::input::onnx::{InferenceError, ModelInference, Tensor};
    use crate::lifecycle::idle::InteractionTimer;

    #[derive(Clone)]
    struct StaticInference {
        outputs: Vec<Tensor>,
    }

    impl ModelInference for StaticInference {
        fn run(&mut self, _input: &Tensor, out: &mut Vec<Tensor>) -> Result<(), InferenceError> {
            out.clone_from(&self.outputs);
            Ok(())
        }
    }

    /// A headless app with the plugin, an interaction timer, image assets,
    /// and injected mock camera + inference.
    fn body_app(detector: Vec<Tensor>, landmark: Vec<Tensor>) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<Image>();
        app.init_resource::<InteractionTimer>();
        app.add_plugins(BodyTrackingPlugin);
        {
            let worker = app.world().resource::<BodyTrackingWorker>();
            let mut frame = Frame::default();
            frame.fit_to(64, 48);
            *worker
                .injected_source
                .lock()
                .expect("source slot") = Some(Box::new(MockFrameSource::looping(vec![frame])));
            *worker
                .injected_pipeline
                .lock()
                .expect("pipeline slot") = Some(Box::new(move || {
                Ok((
                    PosePipeline::new(
                        Box::new(StaticInference { outputs: detector }),
                        Box::new(StaticInference { outputs: landmark }),
                        PoseConfig::default(),
                    ),
                    "mock/backend",
                ))
            }));
        }
        app
    }

    /// Update until `pred` holds or ~2 s elapse (the worker is asynchronous).
    fn update_until(app: &mut App, pred: impl Fn(&World) -> bool) -> bool {
        for _ in 0..200 {
            app.update();
            if pred(app.world()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn request_starts_worker_and_publishes_state_mask_edges_and_presence() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
        });

        let tracked = update_until(&mut app, |w| w.resource::<BodyTrackingState>().present);
        assert!(tracked, "state never reported a person");

        let world = app.world();
        let state = world.resource::<BodyTrackingState>();
        assert!(state.confidence > 0.8);
        assert!(state.landmarks[0].pos.x.is_finite());
        assert!(state.landmarks[0].visibility > 0.7);
        assert!((state.world_landmarks[0].x - 0.1).abs() < 1e-4);

        let edges = world.resource::<SilhouetteEdges>();
        assert!(edges.generation > 0, "edges never refreshed");
        assert!(!edges.points.is_empty());

        let mask = world.resource::<MaskTexture>();
        let images = world.resource::<Assets<Image>>();
        let image = images.get(&mask.0).expect("mask image");
        let data = image.data.as_ref().expect("mask image holds CPU data");
        assert!(
            data.iter().any(|&b| b > 200),
            "mask bytes never written to the image"
        );

        let diagnostics = world.resource::<BodyTrackingDiagnostics>();
        assert_eq!(diagnostics.backend, "mock/backend");

        // Presence marked the interaction timer (design decision 1).
        let timer = world.resource::<InteractionTimer>();
        assert!(
            timer.last_interaction() > Duration::ZERO,
            "person-bearing frames must reset the idle timer"
        );
    }

    #[test]
    fn empty_frames_do_not_touch_the_interaction_timer() {
        let mut app = body_app(empty_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
        });
        // Give the worker ample time to stream empty frames.
        for _ in 0..40 {
            app.update();
            std::thread::sleep(Duration::from_millis(5));
        }
        let world = app.world();
        assert!(!world.resource::<BodyTrackingState>().present);
        assert_eq!(
            world.resource::<InteractionTimer>().last_interaction(),
            Duration::ZERO,
            "empty frames must never reset the idle timer"
        );
    }

    #[test]
    fn removing_the_request_stops_the_worker_and_clears_state() {
        let mut app = body_app(hot_person_detector_outputs(), confident_landmark_outputs());
        app.insert_resource(BodyTrackingRequest {
            idle_throttle: false,
        });
        assert!(update_until(&mut app, |w| w
            .resource::<BodyTrackingState>()
            .present));

        app.world_mut().remove_resource::<BodyTrackingRequest>();
        app.update();

        let world = app.world();
        assert!(!world.resource::<BodyTrackingState>().present);
        assert!(world.resource::<SilhouetteEdges>().points.is_empty());
        let worker = world.resource::<BodyTrackingWorker>();
        assert!(
            worker.runtime.lock().expect("runtime lock").is_none(),
            "worker must be stopped and joined on request removal"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body::systems`
Expected: FAIL — compile error (module missing).

- [ ] **Step 3: Write minimal implementation**

Add `pub mod systems;` to `body/mod.rs`. Create `crates/wc-core/src/input/body/systems.rs`:

```rust
//! Main-thread systems: request-driven worker lifecycle, ring drain,
//! poll-rate smoothing, resource publication, and the presence→idle hook.
//!
//! Both systems are cheap no-ops while no [`super::BodyTrackingRequest`]
//! exists (an early-out on an absent resource / empty runtime), which is the
//! sanctioned always-on-listener shape: they must observe the request's
//! insertion in every app state, so they gate internally rather than on
//! `sketch_active`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rtrb::{Consumer, Producer};

use super::pipeline::BodyLiveTuning;
use super::smoothing::{BodySmoother, DEFAULT_BETA, DEFAULT_MIN_CUTOFF};
use super::transport::{
    seed_payload_pool, BodyFramePayload, BodyWorkerMsg, PAYLOAD_POOL_SIZE, RESULT_RING_CAPACITY,
};
use super::worker::{load_pose_pipeline, spawn_body_worker, SourceFactory, WorkerHandle};
use super::{
    BodyLandmark, BodyTrackingConfig, BodyTrackingDiagnostics, BodyTrackingRequest,
    BodyTrackingState, BodyTrackingStatus, MaskTexture, SilhouetteEdges, BODY_LANDMARK_COUNT,
    MASK_SIZE_U32,
};
use crate::input::capture::{CaptureError, FrameSource};
use crate::lifecycle::idle::InteractionTimer;

/// The latest worker result, held between worker frames as the smoothing
/// target (the worker runs at inference cadence; smoothing runs per poll).
struct BodyTarget {
    present: bool,
    confidence: f32,
    landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    world: [Vec3; BODY_LANDMARK_COUNT],
    timestamp: Duration,
}

impl Default for BodyTarget {
    fn default() -> Self {
        Self {
            present: false,
            confidence: 0.0,
            landmarks: [BodyLandmark::default(); BODY_LANDMARK_COUNT],
            world: [Vec3::ZERO; BODY_LANDMARK_COUNT],
            timestamp: Duration::ZERO,
        }
    }
}

/// Everything that exists only while a request is active.
struct BodyRuntime {
    worker: WorkerHandle,
    consumer: Consumer<BodyWorkerMsg>,
    recycle: Producer<Box<BodyFramePayload>>,
    tuning: Arc<BodyLiveTuning>,
    smoother: BodySmoother,
    target: BodyTarget,
    /// Whether the previous poll published a person — lets the state emit a
    /// single clearing write when the person leaves, then stay quiet.
    had_person: bool,
}

/// Owns the worker runtime. `rtrb` endpoints are `Send` but not `Sync`, and
/// Bevy resources must be `Sync`; the `Mutex` provides that (main-thread-only
/// access, so there is never contention — the same shape as the hand
/// provider's `runtime` field).
#[derive(Resource, Default)]
pub struct BodyTrackingWorker {
    /// `Some` while a request is active.
    pub(crate) runtime: Mutex<Option<BodyRuntime>>,
    /// Test-injected camera source (used instead of opening a webcam).
    #[cfg(test)]
    pub(crate) injected_source: Mutex<Option<Box<dyn FrameSource + Send>>>,
    /// Test-injected pipeline factory (used instead of loading models).
    #[cfg(test)]
    pub(crate) injected_pipeline: Mutex<Option<super::worker::PipelineFactory>>,
}

/// Startup: create the reused `R8Unorm` mask image and publish
/// [`MaskTexture`]. Skipped (with a log line) in bare harnesses without
/// image assets; `poll_body_worker` tolerates the absence.
pub fn init_mask_texture(mut commands: Commands, images: Option<ResMut<Assets<Image>>>) {
    let Some(mut images) = images else {
        tracing::info!("body tracking: no Assets<Image>; MaskTexture disabled (headless)");
        return;
    };
    let image = Image::new_fill(
        Extent3d {
            width: MASK_SIZE_U32,
            height: MASK_SIZE_U32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0_u8],
        TextureFormat::R8Unorm,
        // MAIN_WORLD (CPU bytes rewritten each body frame) + RENDER_WORLD
        // (sampled by the silhouette material).
        RenderAssetUsages::default(),
    );
    commands.insert_resource(MaskTexture(images.add(image)));
}

/// `PreUpdate`: reconcile the worker with the request — start on insertion,
/// stop on removal (join + clear published state), and mirror
/// `idle_throttle` into the shared tuning cell every frame (one Relaxed
/// store; unconditional so a rebuilt worker picks the true state up within
/// one frame, matching the hand provider's mirror rationale).
pub fn sync_body_tracking(
    request: Option<Res<'_, BodyTrackingRequest>>,
    config: Res<'_, BodyTrackingConfig>,
    worker: Res<'_, BodyTrackingWorker>,
    mut state: ResMut<'_, BodyTrackingState>,
    mut edges: ResMut<'_, SilhouetteEdges>,
    mut diagnostics: ResMut<'_, BodyTrackingDiagnostics>,
) {
    let Ok(mut runtime) = worker.runtime.lock() else {
        return;
    };
    match (request, runtime.is_some()) {
        (Some(req), true) => {
            if let Some(rt) = runtime.as_ref() {
                rt.tuning.set_idle_throttle(req.idle_throttle);
            }
            diagnostics.idle_throttled = req.idle_throttle;
        }
        (Some(req), false) => {
            *runtime = Some(start_worker(&worker, &config, req.idle_throttle));
            *diagnostics = BodyTrackingDiagnostics {
                status: BodyTrackingStatus::Starting,
                idle_throttled: req.idle_throttle,
                ..BodyTrackingDiagnostics::default()
            };
            tracing::info!("body tracking: request received, worker starting");
        }
        (None, true) => {
            // Dropping the runtime joins the worker (WorkerHandle::drop).
            *runtime = None;
            *state = BodyTrackingState::default();
            edges.points.clear();
            edges.generation = edges.generation.wrapping_add(1);
            *diagnostics = BodyTrackingDiagnostics::default();
            tracing::info!("body tracking: request removed, worker stopped");
        }
        (None, false) => {}
    }
}

/// `PreUpdate` (after [`sync_body_tracking`]): drain the worker ring, keep
/// the newest frame as the smoothing target, publish
/// [`BodyTrackingState`] / mask bytes / [`SilhouetteEdges`], recycle mask
/// payloads, and mark the idle [`InteractionTimer`] on person-bearing frames
/// (empty frames never mark — same semantics as hand-bearing frames in
/// `reset_on_interaction`; see the plugin doc).
#[allow(clippy::too_many_arguments, reason = "publication fan-out; one system keeps the drain atomic")]
pub fn poll_body_worker(
    time: Res<'_, Time>,
    worker: Res<'_, BodyTrackingWorker>,
    mut state: ResMut<'_, BodyTrackingState>,
    mut edges: ResMut<'_, SilhouetteEdges>,
    mut diagnostics: ResMut<'_, BodyTrackingDiagnostics>,
    mask: Option<Res<'_, MaskTexture>>,
    mut images: Option<ResMut<'_, Assets<Image>>>,
    mut timer: Option<ResMut<'_, InteractionTimer>>,
) {
    let Ok(mut runtime) = worker.runtime.lock() else {
        return;
    };
    let Some(rt) = runtime.as_mut() else {
        return;
    };
    let now = time.elapsed();
    let mut person_frame = false;

    while let Ok(msg) = rt.consumer.pop() {
        match msg {
            BodyWorkerMsg::Frame(mut frame) => {
                if let Some(payload) = frame.payload.take() {
                    // Copy the mask bytes into the shared image (Bevy
                    // re-uploads on mutation; 256 KB is trivial)…
                    if let (Some(mask), Some(images)) = (&mask, images.as_deref_mut()) {
                        if let Some(image) = images.get_mut(&mask.0) {
                            if let Some(data) = image.data.as_mut() {
                                data.copy_from_slice(&payload.mask);
                            }
                        }
                    }
                    // …refill the edge list in place (capacity preserved)…
                    edges.points.clear();
                    edges.points.extend_from_slice(&payload.edges);
                    edges.generation = edges.generation.wrapping_add(1);
                    // …and hand the buffer back to the worker. The recycle
                    // ring is sized pool+1 so this cannot fail; if it ever
                    // did, dropping the box merely shrinks the pool.
                    let _ = rt.recycle.push(payload);
                }
                if frame.present {
                    person_frame = true;
                } else if rt.target.present {
                    // Person left: reset the smoother so a return starts
                    // fresh (no stale momentum), mirroring the hand smoother.
                    rt.smoother.clear();
                }
                rt.target = BodyTarget {
                    present: frame.present,
                    confidence: frame.confidence,
                    landmarks: frame.landmarks,
                    world: frame.world_landmarks,
                    timestamp: frame.timestamp,
                };
            }
            BodyWorkerMsg::Backend(backend) => {
                diagnostics.backend = backend;
                tracing::info!("body inference backend: {backend} (detector+landmark)");
            }
            BodyWorkerMsg::Status(status) => diagnostics.status = status,
            BodyWorkerMsg::Diagnostics(d) => {
                diagnostics.inference_interval = d.inference_interval;
                diagnostics.dropped_frames = d.dropped_frames;
                diagnostics.ring_full_drops = d.ring_full_drops;
                diagnostics.pipeline_errors = d.pipeline_errors;
                diagnostics.idle_throttled = d.idle_throttled;
            }
            BodyWorkerMsg::Error(e) => diagnostics.last_error = Some(e),
            BodyWorkerMsg::CameraFormat(f) => diagnostics.camera_format = Some(f),
        }
    }

    // Presence → interaction: identical semantics to hand-bearing frames in
    // reset_on_interaction (both end in InteractionTimer::mark; empty frames
    // are ignored by construction).
    if person_frame {
        if let Some(timer) = timer.as_mut() {
            timer.mark(now);
        }
        if !rt.had_person {
            tracing::info!("body tracking: person detected");
        }
    }

    // Ease the exposed pose toward the held target every poll so the
    // inference cadence renders as fluid motion.
    if rt.target.present {
        let smoothed = rt.smoother.smooth(&rt.target.landmarks, &rt.target.world, now);
        state.present = true;
        state.confidence = rt.target.confidence;
        state.landmarks = smoothed.landmarks;
        state.world_landmarks = smoothed.world;
        state.velocities = smoothed.velocities;
        state.timestamp = rt.target.timestamp;
        rt.had_person = true;
    } else if rt.had_person {
        // One clearing write when the person leaves, then quiet.
        rt.had_person = false;
        *state = BodyTrackingState::default();
        rt.smoother.clear();
        tracing::info!("body tracking: person lost");
    }
}

/// Build the rings, seed the payload pool, and spawn the worker.
fn start_worker(
    worker: &BodyTrackingWorker,
    config: &BodyTrackingConfig,
    idle_throttle: bool,
) -> BodyRuntime {
    let (result_tx, result_rx) = rtrb::RingBuffer::new(RESULT_RING_CAPACITY);
    let (mut recycle_tx, recycle_rx) = rtrb::RingBuffer::new(PAYLOAD_POOL_SIZE + 1);
    seed_payload_pool(&mut recycle_tx);
    let tuning = Arc::new(BodyLiveTuning::new(
        super::mask::DEFAULT_MASK_EMA_ALPHA,
    ));
    tuning.set_idle_throttle(idle_throttle);

    #[cfg(test)]
    let injected_source = worker
        .injected_source
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    #[cfg(not(test))]
    let injected_source: Option<Box<dyn FrameSource + Send>> = None;
    #[cfg(not(test))]
    let _ = worker; // only the test slots read it here
    let camera_index = config.camera_index;
    let make_source: SourceFactory = match injected_source {
        Some(src) => Box::new(move || {
            let boxed: Box<dyn FrameSource> = src;
            Ok(boxed)
        }),
        None => Box::new(move || open_camera_source(camera_index)),
    };

    #[cfg(test)]
    let injected_pipeline = worker
        .injected_pipeline
        .lock()
        .ok()
        .and_then(|mut slot| slot.take());
    #[cfg(not(test))]
    let injected_pipeline: Option<super::worker::PipelineFactory> = None;
    let make_pipeline = injected_pipeline.unwrap_or_else(|| {
        let model_dir = config.model_dir.clone();
        Box::new(move || load_pose_pipeline(&model_dir))
    });

    let handle = spawn_body_worker(
        make_source,
        make_pipeline,
        config.max_inference_hz,
        Arc::clone(&tuning),
        result_tx,
        recycle_rx,
    );
    BodyRuntime {
        worker: handle,
        consumer: result_rx,
        recycle: recycle_tx,
        tuning,
        smoother: BodySmoother::new(DEFAULT_MIN_CUTOFF, DEFAULT_BETA),
        target: BodyTarget::default(),
        had_person: false,
    }
}

/// Open a real webcam source on the calling (worker) thread, or error. The
/// same per-platform selection as the hand provider, gated on this
/// modality's camera feature.
pub fn open_camera_source(camera_index: u32) -> Result<Box<dyn FrameSource>, CaptureError> {
    #[cfg(all(feature = "body-tracking-camera", target_os = "macos"))]
    {
        let source = crate::input::capture::AvfFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(all(feature = "body-tracking-camera", not(target_os = "macos")))]
    {
        let source = crate::input::capture::NokhwaFrameSource::open(camera_index)?;
        let boxed: Box<dyn FrameSource> = Box::new(source);
        Ok(boxed)
    }
    #[cfg(not(feature = "body-tracking-camera"))]
    {
        let _ = camera_index;
        Err(CaptureError::NoCamera(
            "build with the body-tracking-camera feature".into(),
        ))
    }
}
```

Then finalize the plugin in `body/mod.rs` — replace the Task 3 `impl Plugin` with:

```rust
impl Plugin for BodyTrackingPlugin {
    /// Data flow:
    ///
    /// ```text
    /// BodyTrackingRequest (sketch inserts/removes; idle_throttle mirrors SketchActivity)
    ///   └─ systems::sync_body_tracking   — spawns/stops the worker, mirrors the throttle
    /// worker thread (systems-spawned):
    ///   camera FrameSource → PosePipeline (detector → ROI → landmarks/mask/edges)
    ///     → rtrb result ring (BodyWorkerMsg; mask via the recycled payload pool)
    ///   └─ systems::poll_body_worker     — drains the ring, One-Euro smooths,
    ///        writes BodyTrackingState + MaskTexture bytes + SilhouetteEdges,
    ///        recycles payloads, marks InteractionTimer on person-bearing frames
    /// ```
    ///
    /// Both `PreUpdate` systems run under `InputSystems` (like the hand
    /// subsystem) and are internally gated on the request/runtime: with no
    /// request they are two early-outs per frame — the sanctioned always-on
    /// listener shape (they must observe a request inserted from any state).
    fn build(&self, app: &mut App) {
        app.init_resource::<BodyTrackingState>()
            .init_resource::<BodyTrackingDiagnostics>()
            .init_resource::<SilhouetteEdges>()
            .init_resource::<BodyTrackingConfig>()
            .init_resource::<systems::BodyTrackingWorker>()
            .add_systems(Startup, systems::init_mask_texture)
            .add_systems(
                PreUpdate,
                (systems::sync_body_tracking, systems::poll_body_worker)
                    .chain()
                    .in_set(bevy::input::InputSystems),
            );
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body`
Expected: PASS (the three new app-level tests plus every prior body test). Also `cargo check -p wc-core --features body-tracking-camera` (camera cfg arms) and `cargo check -p waveconductor` (binary default features).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): request-driven systems, mask publication, and presence hook" -m "sync_body_tracking starts/stops the worker on BodyTrackingRequest insert/remove and mirrors idle_throttle each frame; poll_body_worker drains the ring, smooths at poll rate, writes state/mask/edges, recycles payloads, and marks InteractionTimer on person-bearing frames only (hand-frame semantics). Plugin wiring finalized with the data-flow doc." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 13: Model acquisition + CoreML surgery check — OPERATOR-ASSISTED

Downloads the two BlazePose ONNX models, verifies their shapes, checks CoreML partitioning per the runbook, and vendors them with provenance. **Requires the operator** (network access policy + judgment on the partition log). The agent prepares every command; Madison runs/approves the downloads.

**Files:**
- Create: `assets/models/pose/pose_detection.onnx`, `assets/models/pose/pose_landmark_full.onnx`, `assets/models/pose/ATTRIBUTION.md`, `assets/models/pose/LICENSE`

**Interfaces:**
- Consumes: the source URLs below (verified live 2026-07-12).
- Produces: the vendored model files Tasks 14/15 and `BodyTrackingConfig::default().model_dir` rely on.

**Sources (primary — OpenCV zoo HuggingFace mirrors, Apache-2.0, the same lineage as the vendored hand models):**
- Detector: <https://huggingface.co/opencv/person_detection_mediapipe/resolve/main/person_detection_mediapipe_2023mar.onnx> (~12 MB; input `[1,224,224,3]`, outputs `[1,2254,12]` + `[1,2254,1]`)
- Landmark: <https://huggingface.co/opencv/pose_estimation_mediapipe/resolve/main/pose_estimation_mediapipe_2023mar.onnx> (~5.6 MB; input `[1,256,256,3]`, outputs `[1,195]`, `[1,1]`, `[1,256,256,1]` mask, heatmap, `[1,117]` world)

**Alternate (explicit `full` variant, if the mirror's variant proves insufficient on hardware):** the PINTO model zoo `053_BlazePose` folder — <https://github.com/PINTO0309/PINTO_model_zoo/tree/main/053_BlazePose> (`download.sh` / `url.txt` fetch per-variant archives including `full`). **Known ambiguity, resolved as follows:** the OpenCV mirror's README does not state which BlazePose variant (lite/full/heavy) its 5.6 MB ONNX derives from; its size suggests lite-or-full-fp16 heritage. It is adopted anyway because (a) it is the same trusted conversion lineage as the hand models, (b) it demonstrably emits the 33-landmark + 256² mask outputs the spec requires, and (c) the filename `pose_landmark_full.onnx` names the *slot*; if hardware testing (Task 15) shows insufficient tracking quality, re-vendor the explicit full variant from PINTO into the same filename and re-run Task 14 — no code change.

- [ ] **Step 1 (operator): Download the models**

```bash
mkdir -p assets/models/pose
curl -L -o assets/models/pose/pose_detection.onnx 'https://huggingface.co/opencv/person_detection_mediapipe/resolve/main/person_detection_mediapipe_2023mar.onnx'
curl -L -o assets/models/pose/pose_landmark_full.onnx 'https://huggingface.co/opencv/pose_estimation_mediapipe/resolve/main/pose_estimation_mediapipe_2023mar.onnx'
```

- [ ] **Step 2: Verify graph I/O shapes**

```bash
uv run --with onnx python - <<'PY'
import onnx
for name in ("pose_detection", "pose_landmark_full"):
    m = onnx.load(f"assets/models/pose/{name}.onnx")
    g = m.graph
    def dims(vi):
        return [d.dim_value for d in vi.type.tensor_type.shape.dim]
    print(name, "inputs:", [(i.name, dims(i)) for i in g.input])
    print(name, "outputs:", [(o.name, dims(o)) for o in g.output])
PY
```

Expected: detector input `[1,224,224,3]` (NHWC), outputs `[1,2254,12]` and `[1,2254,1]`; landmark input `[1,256,256,3]`, outputs including `[1,195]`, `[1,1]`, `[1,256,256,1]`, and `[1,117]` (a heatmap-shaped extra is fine — the pipeline picks by shape). **STOP and re-plan if the input is NCHW (`[1,3,H,W]`) or the shapes disagree** — the pipeline assumes NHWC like every model in this family.

- [ ] **Step 3: CoreML partition check (macOS; per the runbook)**

Follow `docs/runbooks/onnx-coreml-model-surgery.md` §"How to diagnose CoreML fragmentation": clear the cache (`rm -rf ~/Library/Caches/waveconductor/coreml-cache`), then run the runbook's `uv run --with onnxruntime --with numpy` capability-log script against each model. If the log shows `PReLU` rejections with `[1, C, 1, 1]` slope shapes and a partition count far above single digits, apply the runbook's bit-exact slope reshape (`[1,C,1,1] → [C,1,1]`, verified CPU-EP diff `0.0` before writing) — the known BlazePose-family failure mode. The per-model CoreML cache key (`model_cache_key`) already namespaces any edit safely. Record partition counts (before/after if surgery ran) in ATTRIBUTION.md.

- [ ] **Step 4: Write provenance**

Create `assets/models/pose/ATTRIBUTION.md` (mirroring `assets/models/hand/ATTRIBUTION.md`): source URLs, upstream filenames, SHA-256 of each vendored file (`shasum -a 256 assets/models/pose/*.onnx`), license (Apache-2.0), download date, any surgery applied (with the diff-0.0 verification note), and the variant-ambiguity note above. Copy the Apache-2.0 text as `assets/models/pose/LICENSE` (the HF repos ship it).

- [ ] **Step 5: Gate and commit**

Run: `cargo xtask check-secrets` (the new files are binary + docs; must stay clean) and `cargo deny check` (no dependency change — should be untouched).

```bash
git add assets/models/pose
git commit -m "feat(assets): vendor BlazePose person-detection and pose-landmark models" -m "OpenCV zoo HuggingFace mirrors (Apache-2.0), shapes verified (2254-anchor detector, 39-row landmark model with 256x256 segmentation mask and world landmarks). CoreML partition check per docs/runbooks/onnx-coreml-model-surgery.md recorded in ATTRIBUTION.md." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 14: Real-model integration tests

Pin the vendored models' contracts the way `input/onnx/ort.rs`'s tests pin the hand models: shapes, backend registration, and the presence-scalar-is-a-probability premise from design decision 6.

**Files:**
- Modify: `crates/wc-core/src/input/body/worker.rs` (append a `model_tests` module to the existing `#[cfg(test)]` footer area)

**Interfaces:**
- Consumes: `load_pose_pipeline`, `OrtInference`, the vendored models from Task 13.

- [ ] **Step 1: Write the failing test**

Append inside `worker.rs` (as a sibling of `mod tests`):

```rust
/// Contract tests against the VENDORED pose models (Task 13). These require
/// assets/models/pose to be populated — they are ordered after the model
/// acquisition task and fail loudly (like the hand-model tests) if the
/// assets are missing.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod model_tests {
    use std::path::PathBuf;

    use super::*;
    use crate::input::onnx::ort::OrtInference;
    use crate::input::onnx::{ModelInference, Tensor};

    fn model_bytes(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/models/pose")
            .join(name);
        std::fs::read(path).expect("read vendored pose model (run Task 13 first)")
    }

    #[test]
    fn pose_detector_emits_2254_anchor_tensors() {
        let mut model =
            OrtInference::load(&model_bytes(POSE_DETECTION_MODEL)).expect("load detector");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 224, 224, 3]), &mut out)
            .expect("detector forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        assert!(shapes.contains(&[1, 2254, 12].as_slice()), "shapes={shapes:?}");
        assert!(shapes.contains(&[1, 2254, 1].as_slice()), "shapes={shapes:?}");
    }

    #[test]
    fn pose_landmark_emits_landmarks_confidence_mask_and_world() {
        let mut model =
            OrtInference::load(&model_bytes(POSE_LANDMARK_MODEL)).expect("load landmark");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 256, 256, 3]), &mut out)
            .expect("landmark forward pass");
        let shapes: Vec<&[usize]> = out.iter().map(|t| t.shape.as_slice()).collect();
        for want in [
            [1_usize, 195].as_slice(),
            [1, 1].as_slice(),
            [1, 256, 256, 1].as_slice(),
            [1, 117].as_slice(),
        ] {
            assert!(shapes.contains(&want), "missing {want:?} in {shapes:?}");
        }
    }

    #[test]
    fn pose_confidence_is_a_probability_from_the_graph() {
        // Premise lock (design decision 6): the pose-presence scalar is
        // consumed RAW. An all-zeros input contains no person, so a baked-in
        // sigmoid must read in [0, 1] and low; raw logits would violate the
        // range loudly here before the pipeline silently misreads them.
        let mut model =
            OrtInference::load(&model_bytes(POSE_LANDMARK_MODEL)).expect("load landmark");
        let mut out = Vec::new();
        model
            .run(&Tensor::zeros(vec![1, 256, 256, 3]), &mut out)
            .expect("landmark forward pass");
        let conf = out
            .iter()
            .find(|t| t.shape == [1, 1])
            .and_then(|t| t.data.first().copied())
            .expect("confidence scalar");
        assert!(
            (0.0..=1.0).contains(&conf),
            "confidence {conf} outside [0, 1] — head is not pre-activated; \
             the pipeline must sigmoid it (update pick_pose_landmark_outputs)"
        );
        assert!(conf < 0.5, "empty input should read low, got {conf}");
    }

    #[test]
    fn load_pose_pipeline_reports_a_known_backend() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/models/pose");
        let (_pipeline, backend) = load_pose_pipeline(&dir).expect("pipeline builds");
        #[cfg(target_os = "macos")]
        assert_eq!(backend, "ort/CoreML", "CoreML must register on macOS");
        #[cfg(not(target_os = "macos"))]
        assert!(!backend.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe model_tests`
Expected: FAIL only if Task 13 was skipped (missing assets) — otherwise these should pass on first run; the "failing" state for this task is running them **before** writing them (they don't exist). If `pose_confidence_is_a_probability_from_the_graph` fails on the range assertion, the model ships raw logits: apply sigmoid to `confidence` in `pick_pose_landmark_outputs` (one line: `confidence: super::detector::sigmoid(confidence)`) and update design-decision 6's comment — then re-run.

- [ ] **Step 3: (covered above — the test IS the deliverable; adjust the pipeline only if the probability premise fails)**

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p wc-core --features body-tracking-mediapipe input::body`
Expected: PASS, including the 4 model tests (CoreML label on macOS proves the EP registered against the vendored models).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(body): pin vendored pose-model contracts" -m "Shape contracts for both vendored models, the raw-probability premise for the pose-presence scalar, and CoreML backend registration on macOS - mirroring the hand-model contract tests." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 15: Live-camera probe example + smoke test — OPERATOR-ASSISTED

A worker-level probe binary (no Bevy app needed) so the operator can validate the full camera → detector → landmark → mask/edges path on real hardware. This is Plan B's smoke test: inside `cargo rund` the plugin is deliberately inert until Plan C inserts `BodyTrackingRequest`, so there is nothing to see in-app yet.

**Files:**
- Create: `crates/wc-core/examples/body_tracking_probe.rs`
- Modify: `crates/wc-core/Cargo.toml` (example entry)

**Interfaces:**
- Consumes: the public `body::worker` / `body::transport` / `body::pipeline` API and `systems::open_camera_source`.

- [ ] **Step 1: Add the example entry**

In `crates/wc-core/Cargo.toml`, next to the leap examples:

```toml
[[example]]
name = "body_tracking_probe"
required-features = ["body-tracking-camera"]
```

- [ ] **Step 2: Write the probe**

Create `crates/wc-core/examples/body_tracking_probe.rs`:

```rust
//! Live-camera probe for the BlazePose body-tracking worker.
//!
//! Opens the default webcam, loads the vendored pose models, runs the worker
//! for ~30 seconds, and prints a once-per-second summary: presence,
//! confidence, nose position, mask coverage, edge count, backend, and error
//! counters. No Bevy app — this exercises exactly the worker seam the
//! `BodyTrackingPlugin` drives.
//!
//! Run (operator, with a camera attached):
//!
//! ```sh
//! cargo run -p wc-core --example body_tracking_probe --features body-tracking-camera
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use wc_core::input::body::pipeline::BodyLiveTuning;
use wc_core::input::body::systems::open_camera_source;
use wc_core::input::body::transport::{seed_payload_pool, BodyWorkerMsg};
use wc_core::input::body::worker::{
    load_pose_pipeline, spawn_body_worker, PipelineFactory, SourceFactory,
};
use wc_core::input::body::{landmark_index, MASK_SIZE};

const RUN_FOR: Duration = Duration::from_secs(30);

fn main() {
    let model_dir = wc_core::platform::assets::asset_root().join("models/pose");
    println!("body_tracking_probe: models from {}", model_dir.display());

    let (result_tx, mut result_rx) = rtrb::RingBuffer::new(64);
    let (mut recycle_tx, recycle_rx) = rtrb::RingBuffer::new(4);
    seed_payload_pool(&mut recycle_tx);
    let tuning = Arc::new(BodyLiveTuning::new(0.35));

    let make_source: SourceFactory = Box::new(|| open_camera_source(0));
    let make_pipeline: PipelineFactory = Box::new(move || load_pose_pipeline(&model_dir));
    let mut handle = spawn_body_worker(
        make_source,
        make_pipeline,
        30,
        tuning,
        result_tx,
        recycle_rx,
    );

    let start = Instant::now();
    let mut next_report = start + Duration::from_secs(1);
    let mut frames = 0_u64;
    let mut present = false;
    let mut confidence = 0.0_f32;
    let mut nose = (0.0_f32, 0.0_f32);
    let mut coverage_pct = 0.0_f32;
    let mut edge_count = 0_usize;
    let mut backend = "?";
    let mut errors = 0_u64;

    while start.elapsed() < RUN_FOR {
        while let Ok(msg) = result_rx.pop() {
            match msg {
                BodyWorkerMsg::Frame(mut frame) => {
                    frames += 1;
                    present = frame.present;
                    confidence = frame.confidence;
                    let n = frame.landmarks[landmark_index::NOSE].pos;
                    nose = (n.x, n.y);
                    if let Some(payload) = frame.payload.take() {
                        let lit = payload.mask.iter().filter(|&&b| b >= 128).count();
                        coverage_pct = pct(lit, MASK_SIZE * MASK_SIZE);
                        edge_count = payload.edges.len();
                        let _ = recycle_tx.push(payload);
                    }
                }
                BodyWorkerMsg::Backend(b) => backend = b,
                BodyWorkerMsg::Status(s) => println!("status: {}", s.label()),
                BodyWorkerMsg::Diagnostics(_) => {}
                BodyWorkerMsg::Error(e) => {
                    errors += 1;
                    eprintln!("error: {e}");
                }
                BodyWorkerMsg::CameraFormat(f) => println!("camera: {f}"),
            }
        }
        if Instant::now() >= next_report {
            next_report += Duration::from_secs(1);
            println!(
                "[{:>2}s] backend={backend} frames={frames} present={present} \
                 conf={confidence:.2} nose=({:.2},{:.2}) mask={coverage_pct:.1}% \
                 edges={edge_count} errors={errors}",
                start.elapsed().as_secs(),
                nose.0,
                nose.1,
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.stop();
    println!("done.");
}

/// Percentage of `part` in `whole` (probe display only; counts ≤ 65536).
fn pct(part: usize, whole: usize) -> f32 {
    let p = u16::try_from(part.min(usize::from(u16::MAX)))
        .map(f32::from)
        .unwrap_or(f32::MAX);
    let w = u16::try_from(whole.min(usize::from(u16::MAX)))
        .map(f32::from)
        .unwrap_or(1.0);
    if w > 0.0 {
        p * 100.0 / w
    } else {
        0.0
    }
}
```

- [ ] **Step 3: Verify it builds (deferred build)**

Run: `cargo check -p wc-core --features body-tracking-camera --examples`
Expected: clean.

- [ ] **Step 4 (operator): Live smoke test**

Prompt Madison to run, with a webcam attached and `assets/models/pose/` populated:

```bash
cargo run -p wc-core --example body_tracking_probe --features body-tracking-camera
```

Acceptance checklist (operator judgment):
- `backend=ort/CoreML` on the Mac (a CPU label here is the silent-fallback symptom — stop and check the CoreML partition log per the runbook).
- `present=true` with `conf ≥ ~0.8` while standing in frame; `present=false` after stepping out.
- `nose=(x, y)` tracks head motion smoothly in `0..1`, x increasing when moving toward the camera's right (unmirrored camera space — mirroring is Plan C).
- `mask=…%` rises to roughly body-sized coverage (a few % to ~20 % depending on distance) and falls toward 0 after leaving (EMA fade).
- `edges=…` in the hundreds while present, bounded at 2048.
- `frames` advances at ~30/s; `errors=0`.

Record observed numbers in the task journal / PR notes. First run compiles the CoreML artifacts (a few seconds of startup latency is expected once per model).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(body): live-camera probe example for operator smoke testing" -m "Worker-level probe printing presence, confidence, nose position, mask coverage, edge count, backend label, and error counters at 1 Hz - the Plan B hardware acceptance tool until the Radiance sketch (Plan C) exists." -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 16: Full gate sweep + final review

**Files:** none (verification only; fix anything the gates surface, amending the responsible module).

- [ ] **Step 1: Run every CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all clean. Notes for likely findings:
- `--all-features` compiles hand + body + camera + templates together — watch for `cfg` overlap mistakes in `input/capture` (the `any(...)` gates from Task 1).
- The doc gate builds DEFAULT features: the body module is not documented there, so the risk is a stray intra-doc link FROM non-gated code (e.g. `lib.rs`, `input/mod.rs`) INTO gated items — those must be plain code spans.
- nextest runs the hand-model and pose-model tests in parallel processes; the CoreML on-disk cache is already `cfg(test)`-disabled in `onnx/ort.rs`, so no cache races.

- [ ] **Step 2: Self-review checklist (fix, don't just note)**

- Pinned contracts (`radiance-shared-contracts.md`) reproduced verbatim: `BODY_LANDMARK_COUNT`/`MAX_EDGE_POINTS`/`MASK_SIZE`; `BodyTrackingRequest { idle_throttle }`; `BodyLandmark { pos, visibility }`; `BodyTrackingState { present, confidence, landmarks, world_landmarks, velocities, timestamp }`; `MaskTexture(pub Handle<Image>)`; `EdgePoint { pos, normal }` (`#[repr(C)]`, `Pod`, `Zeroable`); `SilhouetteEdges { points, generation }`. Additive fields only — nothing renamed or retyped.
- Idle rule: with no request, the two `PreUpdate` systems early-out; with a request in `Idle`/`Screensaver`, the worker is detector-only at 4 Hz with the hardware capture throttle.
- Hot paths: grep the worker loop, `PosePipeline::process`, `MaskProcessor`, `extract_edges`, `BodySmoother::smooth`, and `poll_body_worker` for `Vec::new`/`vec![`/`String`/`format!`/`to_owned`/`collect` — allocations allowed only on error paths and at construction.
- Rustdoc: every `pub` item in `input/body/`, `input/capture/`, `input/onnx/` carries `///`; every module has `//!`; `BodyTrackingPlugin::build` documents the data flow.
- No home-directory paths anywhere (model paths come from `asset_root()`; tests use `CARGO_MANIFEST_DIR`).

- [ ] **Step 3: Commit (only if fixes were needed)**

```bash
git add -A
git commit -m "chore(body): gate-sweep fixes for the body-tracking plan" -m "Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## Plan self-review: Unit B spec coverage

| Spec requirement (Unit B) | Where |
| --- | --- |
| Capture promotion to `input/capture/`, hand provider keeps compiling | Task 1 (alias `use crate::input::capture;`, `IDLE_INFERENCE_HZ` relocation) |
| Parallel seam, no premature trait; `BodyTrackingPlugin` + one worker | Tasks 3, 11, 12 |
| Worker copies mediapipe shape: `SourceFactory`, rtrb ring, newest-frame-wins undecoded drop, idle throttle, atomics | Task 11 (budget-before-capture loop, edge-triggered capture throttle, `BodyLiveTuning`) |
| BlazePose two-stage: 224² detector → ROI → 256² landmark (`full`) with 33 landmarks + world + 256² mask | Tasks 4, 5, 9; variant note in Task 13 |
| Models in `assets/models/pose/` via `asset_root()` | Task 3 (`BodyTrackingConfig`), Task 13 |
| Model acquisition: PINTO `053_BlazePose` / OpenCV HF mirror URLs (web-verified), operator-assisted download, surgery check per runbook | Task 13 |
| Anchor decode + NMS in Rust; pose anchor config documented with real numbers | Task 4 + design decision 4 (2254 = 28²·2 + 14²·2 + 7²·6) |
| Detect-then-track (detector re-runs only when track lost) | Task 9 (`tracked: Option<RoiRect>`, aux rows 33/34) |
| Ring transport: POD landmarks; mask via two-ring buffer pool, steady-state alloc-free | Task 8 (pool tests pin pointer reuse), Task 11 |
| Pinned `BodyTrackingState` / `MaskTexture` (R8Unorm 256², written in place) / `SilhouetteEdges` (≤2048 `(pos, normal)`, worker-side, single pass, fixed capacity) | Tasks 3, 7, 12 |
| One-Euro landmarks (poll rate) + worker-side mask EMA | Tasks 10, 6 |
| Activation by insert/remove of `BodyTrackingRequest`; `idle_throttle` → detector-only + capture throttle | Tasks 12, 11, 9 (probe path) |
| Presence resets `InteractionTimer` like hand-bearing frames; mechanism documented | Design decision 1 + Task 12 (`poll_body_worker`), tested both ways (presence marks, empty never marks) |
| Feature flags `body-tracking-mediapipe` / `body-tracking-camera`, CI `--all-features` clean, doc-gate safe | Tasks 1, 3, 16 |
| Diagnostics: backend label (CPU fallback visible), status enum, worker error propagation | Tasks 3, 11, 12, 14 |
| Rustdoc everywhere + `build()` data flow | Every task; Task 12 plugin doc; Task 16 checklist |
| Headless tests: mask EMA; edge extraction (circle, torso, capacity clamp, outward normals); pool recycling (no alloc after init); anchor decode fixtures; landmark projection; One-Euro reuse; `MockFrameSource` everywhere a source is needed | Tasks 6, 7, 8, 4, 5, 10, 9, 11, 12 |
| Camera arbitration vs MediaPipe hands | **Deliberately out of scope** — the pinned contracts assign it to Plan C ("camera arbitration vs the MediaPipe hand provider" under *Plan C consumes/owns*); Plan B's `BodyTrackingConfig.camera_index` and request contract are the seam it uses |
| Fallback model (MoveNet Lightning) | Documented spec-side; not built in v1 (spec) — no task, by design |

Placeholder scan: no TBDs; every code step is complete Rust; the two operator tasks (13, 15) are explicit about which steps need Madison. Type-consistency: all pinned names/types match `radiance-shared-contracts.md` exactly; additions (`MASK_SIZE_U32`, diagnostics/config/status types, `landmark_index`, `SmoothedBody`, transport/worker internals) are additive only.
