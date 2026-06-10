# WaveConductor v5 — MediaPipe Webcam Hand-Tracking Provider

**Date:** 2026-06-04
**Workstream:** New roadmap item — proposed slug `mediapipe-webcam-hands` (see *Roadmap entry* below)
**Status:** Functionally complete enough to run on branch
`mediapipe-hand-tracking`, but **not merge-ready after the 2026-06-07
reassessment**. The branch now has a working Rust-owned two-stage pipeline plus
an optional `ort`/CoreML backend, but reliability/performance still lag the
first-party MediaPipe demos. Remaining work is no longer just "hardware tuning":
fix tracking-ROI graph parity, add stage/frame-age diagnostics, restore
drop-not-sleep rate limiting, cap camera format from enumerated modes, remove
hot-path copies/allocations, and fix zero `palm_velocity` before acceptance.
**Scope window:** ~6–9 focused days (provider plumbing + two-stage ONNX glue + verification spike + diagnostics)
**Branch:** `mediapipe-hand-tracking` (off `v5-alpha`; merges back to `v5-alpha` on Madison's sign-off)

## Goal

Add a second real hand-tracking provider that derives 21-landmark hands from a
**conventional USB webcam** using Google's MediaPipe hand models, running
**fully in-process with Rust-owned orchestration** — no Leap Motion device, no
external process, no Python at runtime. The webcam provider slots into the
existing `HandTrackingProvider` seam as `ProviderId::MediaPipe`, emits frames in
the **same coordinate convention the Leap provider uses**, and therefore drives
every existing sketch (Line attractors, HandMesh, gesture detection, pointer
merge) with **zero changes to consumer code**.

This unlocks hand-tracking on any machine with a webcam — broadening the kiosk's input options beyond the Leap Motion Controller, and serving as the desktop sibling of the roadmap's Phase-3 `ios-vision-hands` (camera-based hands on iPad). It is also a deliberate skills/portfolio investment: WaveConductor owns the full inference path — the two-stage palm→ROI→landmark pre/post-processing glue and the ONNX runtime integration — rather than calling a turnkey black box.

## Background: architecture decision record

Two architectural forks were resolved by senior-engineer subagent debate to consensus (per the project goal directive), with Madison concurring on the first.

### Fork 1 — Integration strategy (decided: **in-process Rust**)

| Path | What | Verdict |
|------|------|---------|
| **A. In-process Rust** | `nokhwa` webcam capture + an ONNX runtime running the converted MediaPipe models, glue in Rust. Single self-contained binary. | **CHOSEN.** Matches the in-process Leap house style, one PID to supervise for the multi-hour unattended soak, the existing `ProviderRegistry` + Mock-fallback acts as supervisor, and Madison explicitly wants to work closer to the ML models. |
| **B. Python MediaPipe sidecar** | A `uv`-managed Python process runs Google's first-party `HandLandmarker` and streams landmarks over the existing `websocket.rs` provider. | Rejected. Its only decisive edges were speed-to-demo (no hard deadline exists) and zero porting risk (neutralized by the numeric oracle below). Adds a permanent second process + Python runtime + systemd unit to an otherwise single-binary deploy. The Path-B advocate itself concluded "I recommend Path A." Retained **only as a dev-time validation oracle** (see below). |
| **C. WasmEdge `mediapipe-rs`** | The maintained `WasmEdge/mediapipe-rs` crate (full two-stage hand pipeline). | Rejected for native embedding. The crate runs **only** as `wasm32-wasi` inside the WasmEdge VM (maintainer-confirmed; the `wasi-nn` crate panics on native targets). Using it means embedding a whole WASM runtime + WASI-NN plugin + two TFLite shared libs beside the binary and copying every camera frame across the WASM boundary — the opposite of a single clean binary. Its excellent, complete glue is instead used as a **porting reference** for our Rust implementation. |

### Course correction (2026-06-07): keep Rust orchestration, stop calling every backend Rust-native

The current recommendation is **do not switch production to a Python sidecar yet**.
Keep the provider in-process and keep the MediaPipe graph harness in Rust, because
the biggest open risks are still graph-parity, flow-control, capture, and
instrumentation problems that a runtime swap alone will not fix. Use first-party
Python MediaPipe as an oracle/A-B harness and as a temporary demo fallback only,
not as the default kiosk architecture.

Also tighten the stack language: the `tract` backend is Rust-native, but the
`hand-tracking-mediapipe-ort` path is a Rust-owned wrapper around native ONNX
Runtime/CoreML. That is acceptable as a performance backend behind
`HandInference`; it should not be presented as a pure Rust-native stack.

### Fork 2 — ONNX runtime (decided: **`tract`-first, `ort` fallback, behind a thin trait, gated by a verification spike**)

Both runtime advocates independently converged on the same resolution:

- **Shared finding that de-risks `tract`:** both ONNX graphs (`opencv/palm_detection_mediapipe`, `opencv/handpose_estimation_mediapipe`) output **raw tensors** — anchor decode and NMS happen in external Rust glue we write regardless, **not inside the graph**. The operators that usually defeat pure-Rust runtimes (`NonMaxSuppression`, `TopK`) are therefore **absent**. `tract`'s only real exposure is the palm detector's FPN `Resize`/upsample node (nearest-neighbor 2×, which `tract` supports).
- **Decision:** use **`tract`** (pure-Rust, no native C++ blob, `cargo deny`-trivial, statically linked → true single binary on all three desktop OSes, strongest "own the whole stack" learning story) as the primary runtime, **behind a thin `HandInference` trait**, with **`ort` (ONNX Runtime) as a documented fallback** if `tract` cannot run the Resize node economically. The trait makes the fallback a localized swap, not a rewrite.
- **Gate:** a **day-one verification spike** (see *Verification spike* §) loads both models in `tract`, runs a forward pass on a fixture frame, and diffs against a Python `onnxruntime` oracle within `1e-3`. If `tract` passes, it is the runtime. If the lone Resize node fails irreparably and graph surgery is uneconomical, switch the trait's default impl to `ort`. **This decision is empirical, not assumed.**

Why `tract` is favored for *this* project specifically: the roadmap makes **macOS, Linux, and Windows all first-class desktop targets**. `ort` would require vendoring and rpath-wiring a native `libonnxruntime` per platform (three blobs, a `cargo deny` SOURCES/license gap closed by a manual NOTICE, and an rc-version pin). `tract` is one pure-Rust dependency on all three. Combined with the "Hand-Z is not required" roadmap ruling (2026-05-30) — which removes any pressure for hardware-accelerated high-precision depth — the pure-Rust CPU path is comfortably sufficient for two ~2–5 MB models at a capped inference rate.

### Considered alternative — Burn (evaluated 2026-06-04, post-implementation)

[Burn](https://github.com/tracel-ai/burn) (0.21) was evaluated retrospectively as a third runtime. Verdict: **not better for this use case; tract kept.**

- **Where Burn would win:** its ONNX importer (`burn-onnx`) natively handles the `Resize` `linear`/`half_pixel`/`sizes`-input form — the exact node that needed our one `sizes→scales` graph-surgery in tract — so it would likely import both models with **zero surgery** (Burn's official ONNX conformance suite marks `test_resize_upsample_sizes_linear` as passing; not independently verified on our two models). That advantage is moot here: our surgery was one bit-exact, validated rewrite.
- **Where Burn loses/draws for us:** it is a heavier, faster-churning **training** framework (0.21 alone renamed `burn-import`→`burn-onnx` and swapped `burn-ndarray`→`burn-flex`), vs tract's lean, stable, inference-only runtime — worse on footprint/maturity/auditability for two tiny models. Pure-Rust single binary is a **draw** (`burn-flex` CPU). Burn's headline feature — sharing Bevy's wgpu `Device` to run inference on the GPU — is a **non-benefit we'd decline**, since thermal stability is the #1 goal and we deliberately avoid GPU contention with the particle sketches; on CPU it's a draw. Burn's build-time codegen (`build.rs`) is also more rigid than tract's runtime model loader (no model swap without a recompile).
- **When to revisit Burn:** if WaveConductor later wants **on-GPU ML sharing Bevy's device** (a larger generative/segmentation model feeding the visuals), **on-device training/fine-tuning** (e.g. per-installation hand calibration), or models exceeding tract's op coverage. The `HandInference` trait keeps any future runtime swap localized.

## Scope

### In scope

- **`MediaPipeProvider`** implementing `HandTrackingProvider`, living in a new `crates/wc-core/src/input/providers/mediapipe/` module directory (one concept per file, per AGENTS.md).
- **Two-stage MediaPipe hand pipeline in Rust:** SSD palm detection (anchor generation, sigmoid score decode, weighted NMS) → ROI crop+rotation → 21-landmark regression → projection back to image space. Ported from the readable reference glue in `WasmEdge/mediapipe-rs` and `PINTO0309/hand-gesture-recognition-using-onnx` (both Apache/MIT).
- **`HandInference` runtime trait** with a `tract` implementation (primary) and a documented seam for an `ort` implementation (fallback). One model session per stage, pre-allocated, reused.
- **`FrameSource` capture trait** with a `nokhwa` implementation (production, all three desktop OSes) and a `MockFrameSource` (tests inject fixture frames; no camera needed in CI).
- **Worker-thread architecture:** a dedicated OS thread owns the camera + both inference sessions and runs the pipeline at a capped rate; completed `SmallVec<[Hand; MAX_HANDS]>` frames are pushed onto a lock-free `rtrb` SPSC ring. The Bevy-side `poll()` is a non-blocking drain of that ring (no allocation, no blocking, mirroring how `leap_native` keeps device I/O off the main thread).
- **Coordinate glue** mapping MediaPipe normalized image coordinates into the **Leap-device-millimeter convention** that `projection.rs::palm_to_world` and the Line power model already consume (see *Coordinate mapping* §). A cross-provider test asserts MediaPipe and Leap agree on a known pose.
- **Derived per-hand signals** computed in Rust from landmark geometry: `chirality`, `pinch_strength` (thumb-tip↔index-tip), `grab_strength` (fingertip↔palm closure), `palm_normal`, `palm_velocity` (frame-to-frame, smoothed), 20 `bone_centers` for HandMesh, and a stable cross-frame `id` via a small hand tracker (chirality + palm-proximity association).
- **Provider lifecycle + status mapping** onto the existing `ProviderStatus` axes, interpreted for a webcam (camera-present → `DevicePresence::Attached`; frames flowing → `TrackingFlow::Streaming`; camera missing/open-failure → `Errored`/`Failed`; the Leap-specific `wedged` axis stays `false`). `ProviderDiagnostics` reports camera name, model versions, inference latency, and dropped frames.
- **Feature flag** `hand-tracking-mediapipe`, additive and independent of the leaprs-bearing `hand-tracking-gestures`, fanning out across `wc-core` / `wc-sketches` / `waveconductor`. Exercised by CI's `--all-features`.
- **Startup selection:** extend the `WAVECONDUCTOR_HAND_PROVIDER` match in `main.rs` with a `"mediapipe"` branch registering `MediaPipeProvider` as `ProviderRole::Primary`.
- **Vendored model assets** under `assets/models/hand/` (the two ONNX files + generated SSD anchors + `ATTRIBUTION.md`/`LICENSE`), shipped via the existing asset-deploy mechanism, mirroring the vendored-`libLeapC` precedent.
- **Dev-only Python validation oracle** (`tools/handtrack-oracle/`, `uv`-managed, PEP 723 / `pyproject.toml`) used to run the verification spike and regenerate golden test fixtures. Never shipped, never a CI runtime dependency, fully local (no API spend).
- **Hermetic test suite** (TDD): unit tests for anchors/decode/NMS/ROI math/coordinate mapping/pinch-grab/id-tracker; a committed golden-fixture inference regression test (no Python at CI time); a `MockFrameSource`-driven provider test; a `ProviderRegistry` integration test.
- Fix the **stale `Hand` doc comment** (currently says NDC; reality is Leap-device mm) as part of this work, per the "update stale comments" rule.

### Out of scope (deferred)

- **`ort` as primary.** Only adopted if the spike rejects `tract`. The trait seam keeps it cheap to revisit.
- **GPU/Neural-Engine inference acceleration** (CoreML/CUDA EPs). Unnecessary for these tiny models at a capped rate; "Hand-Z not required" removes the precision pressure. Revisit only if a future high-rate/large-model need appears.
- **Hand depth (z) precision.** Per the roadmap, hand-Z is not required. z is emitted best-effort (a coarse hand-scale depth proxy mapped into the expected mm range, with a documented fixed-nominal fallback) so the Line power model gets a sane value; it is not tuned to physical accuracy in this plan.
- **`auto` provider-chain change.** `auto` keeps its current Leap→Mock behavior; MediaPipe is selected explicitly via the env var. A webcam-in-`auto` fallback chain can come later.
- **Multi-provider fusion** (Leap + MediaPipe simultaneously). `fuse_hand_frames` already passes through a single primary; real fusion is its own item.
- **wasm32/web build.** The `websocket.rs` stub remains the web path; `nokhwa`/`tract` native capture is desktop-only here.
- **8-hour on-hardware soak.** That is the deployment dress-rehearsal gate, not this plan; this plan adds the worker/ring instrumentation that the soak will exercise.
- **Persisted MediaPipe settings UI** beyond a minimal camera-index + mirror toggle. Richer tuning (confidence thresholds, rate cap) lands if needed.

## Architecture

### Module layout

```
crates/wc-core/src/input/providers/mediapipe/
├── mod.rs          # MediaPipeProvider: HandTrackingProvider impl. Owns the
│                   #   worker handle + rtrb consumer; poll() drains the ring;
│                   #   status()/diagnostics() read a shared snapshot. Feature-
│                   #   gated #[cfg(feature = "hand-tracking-mediapipe")].
├── worker.rs       # Background thread: owns FrameSource + HandInference + the
│                   #   pipeline; capture→infer→derive loop at a capped rate;
│                   #   pushes Hand frames + status snapshots to the main thread.
├── capture.rs      # FrameSource trait; NokhwaFrameSource (prod) + MockFrameSource
│                   #   (tests). RGB frame into a reused buffer (no per-frame alloc).
├── inference.rs    # HandInference trait; TractInference (primary). Loads the two
│                   #   ONNX sessions; runs a stage given an input tensor.
├── pipeline.rs     # Two-stage orchestration: palm-detect (or reuse prior ROI) →
│                   #   crop/rotate → landmark → assemble raw landmark sets.
├── palm.rs         # Palm-detection post: anchor decode, sigmoid, weighted NMS.
├── anchors.rs      # SSD anchor generation (the GenMediaPipePalmDetectionSSDAnchors
│                   #   equivalent); baked/generated once, asserted against a golden.
├── landmark.rs     # Landmark stage: ROI affine (crop+rotate), de-normalize,
│                   #   project landmarks back to full-image coords.
├── coords.rs       # Image-normalized → Leap-device-mm mapping; mirror; y-flip;
│                   #   z depth proxy. The critical integration glue.
├── signals.rs      # chirality, pinch/grab, palm_normal, palm_velocity,
│                   #   bone_centers, and the cross-frame id tracker.
└── status.rs       # Webcam-flavored ProviderStatus/ProviderDiagnostics mapping.
```

`providers/mod.rs` gains `pub mod mediapipe;` (the module compiles to an empty shell when the feature is off, like `leap_native`).

### Data flow

```
                    worker thread (capped ~20–30 Hz)                 │  Bevy main thread (PreUpdate)
 ┌────────────┐   ┌───────────┐   ┌──────────────┐   ┌───────────┐   │   ┌──────────────────────┐
 │ Nokhwa     │──▶│ palm      │──▶│ ROI crop +   │──▶│ landmark  │   │   │ MediaPipeProvider    │
 │ FrameSource│   │ detect    │   │ rotate (affine)│ │ regress   │   │   │   ::poll() drains ring│
 └────────────┘   │ (tract)   │   └──────────────┘   │ (tract)   │   │   └──────────┬───────────┘
        ▲         └───────────┘          ▲           └─────┬─────┘   │              ▼
        │  reuse prior-frame ROI ────────┘                 ▼         │   Messages<HandTrackingFrame>
        │  when a hand was tracked                  signals.rs:      │   (provider stamped by
        │  (skip palm detect)                       chirality, pinch,│    poll_all_providers)
        │                                           grab, normal,    │              ▼
        │                                           velocity, id,    │   (unchanged) fuse → entities
        │                                           bone_centers,    │   → state → gestures → pointer
        │                                           coords→Leap mm   │              ▼
        │                                                 │          │   (unchanged) sketches
        │                                                 ▼          │
        │                                    rtrb SPSC ring  ────────┼──▶ (non-blocking)
        └──────────────────────────────────────────────────────────┘
```

The MediaPipe-tracking continuity optimization (reuse the previous frame's hand ROI to skip palm detection while a hand stays tracked) is the same trick the real MediaPipe graph uses; it both lowers CPU and improves id stability.

### The `HandInference` runtime trait (Fork-2 seam)

```rust
/// Runs one ONNX model stage. Abstracts the inference runtime so the
/// tract→ort fallback (decided by the day-one spike) is a localized swap.
trait HandInference: Send {
    /// Run the model on a pre-shaped input tensor, returning the raw output
    /// tensors. Pre/post-processing (anchors, NMS, ROI) lives in the pipeline,
    /// not here — keeping this trait runtime-agnostic.
    fn run(&mut self, input: &Tensor) -> Result<Vec<Tensor>, InferenceError>;
}
```

`TractInference` holds the two `tract` `TypedRunnableModel`s. An `OrtInference` (fallback) would hold `ort::Session`s behind the identical trait. The pipeline never names a concrete runtime.

### Coordinate mapping (the critical glue)

The MediaPipe provider **must emit into the same coordinate convention the Leap provider emits** so all downstream consumers (`projection.rs::palm_to_world`, Line's `grab^1.5 · 5^((−z+350)/160)` power model, HandMesh) work unchanged. The empirical target (confirmed from `projection.rs` + `leap_native`) is **Leap-device millimeters**, not NDC:

- **x:** image-normalized `x∈[0,1]`, horizontally **mirrored** (`x_m = 1 - x`; webcam-as-mirror is the natural installation interaction), mapped to `[-200, +200]` mm via `x_mm = (x_m - 0.5) * 400` (frame-left→−200, frame-right→+200).
- **y:** image-normalized `y∈[0,1]` (top=0), mapped to height-above-device `[350, 40]` mm so raising the hand (smaller image-y) → larger mm → screen-top, matching `LEAP_Y_MAX_MM`/`LEAP_Y_MIN_MM`. `y_mm = 350 - y*310`.
- **z:** best-effort depth proxy from apparent hand scale (bbox size as a coarse "closer = bigger" signal) mapped into the mm range the power model expects, with a documented fixed-nominal fallback (≈350 → power factor ≈1.0). Hand-Z is not required (roadmap), so this is intentionally coarse.
- **landmarks:** the 2D screen-relevant landmarks use the same per-axis mapping; the bone-mesh relative geometry uses MediaPipe **world landmarks** (metric, wrist-origin) scaled to mm if the handpose model emits them (verify in the spike) — otherwise reconstructed from image landmarks + the depth proxy.

These mapping constants and the mirror live in `coords.rs` with unit tests pinning known poses, and a cross-provider agreement test.

### Provider status mapping for a webcam

| Axis | Webcam interpretation |
|------|----------------------|
| `service: ServiceConnection` | `NotStarted` → `Connecting` (opening camera) → `Connected` (frames flowing). `Errored` on unrecoverable failure. (No external daemon, so `ServiceMissing` is unused.) |
| `device: DevicePresence` | `Attached` when the camera handle is open; `NoDevice` when no camera enumerated; `Lost` on disconnect mid-run; `Failed` on open error. |
| `health: DeviceHealth` | `STREAMING` while frames flow; `LOW_RESOURCE` if the worker can't hit the rate cap; `BAD_TRANSPORT` on camera read errors. (Leap-only flags like `SMUDGED`/`ROBUST` stay clear.) |
| `streaming: TrackingFlow` | `Streaming { last_frame_ago, dropped_since_start }` from the worker's heartbeat; `NotStreaming` when stalled. |
| `service_health` | empty (no service-side notion for a local webcam). |
| `wedged` | always `false` (Leap-specific). |

`ProviderDiagnostics`: `device_serial` = camera name/index; `sdk_version` = `"MediaPipe (tract) <model-version>"`; `active_policies` = `["mirror", ...]`; `dropped_frames`; `last_error`.

### Feature flags & dependencies

New additive feature, independent of the leaprs-bearing `hand-tracking-gestures`:

```toml
# wc-core/Cargo.toml
[features]
hand-tracking-mediapipe = ["dep:tract-onnx", "dep:nokhwa", "dep:image"]
# image is already in [workspace.dependencies]; add it as an optional dep of
# wc-core so `dep:image` resolves. nokhwa + tract-onnx are new workspace deps.

# waveconductor/Cargo.toml
[features]
default = ["hand-tracking-gestures"]   # unchanged
hand-tracking-mediapipe = [
    "wc-core/hand-tracking-mediapipe",
    "wc-sketches/hand-tracking-gestures",  # ensure sketch hand-consumers compile
]
```

New workspace deps (pinned): `tract-onnx` (pure Rust), `nokhwa` (`input-native`; verify Linux V4L2 build deps on CI day one — flagged risk). `rtrb`, `image`, `smallvec` are already present. **`ort` is NOT added unless the spike selects it.** `cargo deny`/`cargo audit` stay green on the pure-Rust graph; no native blob, no SOURCES/license gap.

> **CI note:** `--all-features` will compile `hand-tracking-mediapipe`. Tests must run **headless** (no camera, no Python): `MockFrameSource` + committed model assets + golden fixtures. Verify `nokhwa`'s `input-native` *builds* (not runs) on the Linux CI runner; if it drags `libv4l` headers, either add them to the CI image or place `nokhwa` behind a `...-camera` sub-feature so the inference/glue stay CI-testable. Resolve during the spike.

### Model assets & licensing

`assets/models/hand/`: `palm_detection.onnx` (~2 MB), `hand_landmark.onnx` (~5 MB), `palm_anchors.bin` (generated once), `ATTRIBUTION.md` + `LICENSE` (both models Apache-2.0, from the OpenCV Zoo conversions). Loaded at runtime via a path resolved relative to the executable/workspace (no hardcoded home paths). ~7 MB committed to git, consistent with the vendored-`libLeapC` precedent; flagged as a size tradeoff (alternative: an `xtask` download step — rejected for offline/reproducible-CI simplicity).

## Verification spike (gates Fork 2)

The **first implementation milestone**, before building the full provider:

1. `tools/handtrack-oracle/` (`uv run`, PEP 723): load both ONNX models in Python `onnxruntime`, run a forward pass on a committed fixture frame, dump per-stage intermediate tensors (anchors, raw palm output, ROI matrix, raw landmark output) to `.npy`. Record golden landmark output.
2. A Rust spike (`xtask` subcommand or an ignored test): load both models in `tract`, run the same fixture, assert output tensors match the oracle within `1e-3`.
3. **Decision point:** `tract` passes → it is the runtime; commit the goldens; proceed. The Resize node fails → attempt a ≤1-day `onnx-graphsurgeon` rewrite (fixed-scale resize); if that also fails → switch the `HandInference` default to `ort` and record the native-lib vendoring + NOTICE tasks. Either way the decision is **recorded in the plan with evidence**.

## Dev-only Python oracle

Per Madison's rules: local-only, `uv`-managed, **no Anthropic/LLM API spend** (this is numeric ML inference, not LLM judgment), surfaced as an `xtask` subcommand for agent-first operation. Two jobs: (a) the spike diff above, (b) regenerate golden inference fixtures when models change. It is a *development* tool — not shipped to the NUC, not a CI runtime dependency. The committed goldens it produces make the CI inference test hermetic.

## Testing strategy (TDD)

- **Unit (pure functions, no I/O):** anchor generation vs golden; box decode; weighted NMS; ROI affine (crop+rotate) math; `coords.rs` mappings (mirror, y-flip, mm ranges, known poses); pinch/grab formulas; palm_normal/velocity; the id-tracker association logic.
- **Golden inference regression:** committed fixture frame + committed golden landmark output (oracle-generated once) → run the real `tract` pipeline → assert within tolerance. Hermetic; no Python, no camera.
- **Provider behavior:** `MockFrameSource` feeding a fixture → `MediaPipeProvider::poll()` emits the expected `HandTrackingFrame`; status transitions (NotStarted→Connecting→Streaming; camera-missing→Errored).
- **Cross-provider agreement:** a synthetic known hand pose produces matching `palm_to_world` results from both the Leap conversion and the MediaPipe conversion (guards the coordinate glue).
- **Registry integration:** mirror `crates/wc-core/tests/input_registry.rs` for the MediaPipe slot.
- **Hot-path discipline:** a test (or soak instrumentation) asserting no allocations in `poll()` after init, consistent with the existing Leap soak.

## Performance & thermal

- Inference runs on a **dedicated worker thread**, never inline in `poll()`. Rate-capped (~20–30 Hz; configurable) — hand tracking does not need 60 Hz. `tract` intra-op threading kept modest to leave headroom for the Bevy render thread.
- `poll()` is a non-blocking `rtrb` drain: no syscalls, no allocation, `SmallVec` inline up to `MAX_HANDS=2`.
- The two models are tiny; expected per-stage CPU latency is low single-digit-to-~10 ms, comfortably within a 30 Hz budget on NUC-class cores with the ROI-reuse optimization skipping palm detection most frames.
- Worker exposes heartbeat + dropped-frame metrics through `ProviderStatus`/`ProviderDiagnostics` so the existing dev panel and the eventual 8-hour soak can observe it. The `cargo xtask capture` visual harness + thermal signal already provide the observation surface.

## Risks & mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `tract` can't run the palm-detector Resize node | Low–Med | The day-one spike catches it immediately (loud load-time failure); ≤1-day graph-surgery rewrite; `ort` fallback behind the trait. |
| Ported glue produces subtly-wrong landmarks | Med | Stage-by-stage `.npy` numeric oracle pins each stage within `1e-3`; the bug is localized to a named stage, not "looks off." |
| `nokhwa` build deps on Linux CI | Med | Verify build (not run) on the CI runner day one; sub-feature-gate the camera if needed so inference/glue stay CI-testable. |
| Coordinate convention mismatch vs Leap | Med | Target is empirically Leap-device mm (confirmed); cross-provider agreement test; mirror `leap_native` conventions exactly. |
| Webcam handedness/world-landmark outputs differ from assumptions | Low–Med | Verified in the spike against the oracle; chirality derivable from landmark geometry as a fallback. |
| ~7 MB models in git | Low | Accepted (matches vendored-`libLeapC`); `xtask`-download is the documented alternative if size becomes a problem. |

## Spike results (2026-06-04)

Run on macOS arm64 with `tract-onnx` 0.21 vs `onnxruntime` 1.26, identical seeded
random inputs, `1e-3` tolerance. Reproducible via `tools/handtrack-oracle/` +
the standalone tract harness.

**Model I/O (resolves open-question 2):**
- `palm_detection`: input `input_1 [1,192,192,3]`; outputs **raw** `[1,2016,18]`
  box/keypoint regressions + `[1,2016,1]` scores. Anchor decode + NMS are
  outside the graph (2016 anchors confirmed) — no `NMS`/`TopK` op present.
- `hand_landmark`: input `input_1 [1,224,224,3]`; outputs `[1,63]` image
  landmarks, `[1,1]` presence, **`[1,1]` handedness**, **`[1,63]` world
  landmarks**. So chirality and metric bone geometry come straight from the
  model — `signals.rs` reads them rather than deriving (simplifies Phase 5).

**tract results:**
- `hand_landmark`: **PASS** — all four outputs match onnxruntime to 1e-4…1e-7.
  tract runs it as-is, no changes.
- `palm_detection`: tract **ignores the `Resize` `sizes` input** and fails at
  `Resize__235` (`[1,256,6,6]` not upsampled to `[1,256,12,12]`). Graph surgery
  (rewrite the 2 FPN `Resize` nodes to `scales=[1,1,2,2]`) makes the shapes
  correct and is **bit-exact under onnxruntime** (0.0 err vs original). After
  surgery, tract runs the model but its `linear`/`half_pixel` Resize **diverges
  from onnxruntime at feature-map edges** (isolated 4×4→8×8 probe: 0.56 max err;
  onnxruntime clamps out-of-range sample coords, tract extrapolates).

**Decision: runtime = `tract`** (both stages run in pure Rust; the landmark
stage that sets final accuracy is bit-perfect). The committed
`assets/models/hand/palm_detection.onnx` is the surgeried, tract-ready model.

**Residual risk RESOLVED (2026-06-04, real-hand validation).** The palm-Resize
edge discrepancy was validated on the canonical MediaPipe hand image and is
**benign**: decoding both runtimes' raw outputs (with the Rust-mirrored
anchor/decode/NMS) gives matching detections — top score onnxruntime 0.829 vs
tract 0.832 (Δ 0.003); top box corners and all 7 keypoints agree to **0.0004
normalized** (sub-pixel). The ~57 divergence on random Gaussian input was the
expected adversarial-high-frequency artifact at feature-map edges; on real
(smooth) images it vanishes. **tract is the runtime, no caveat.** The mitigation
ladder (ConvTranspose decomposition → ort-for-palm → ort-for-both) is retained
in the git history only as a contingency; it is not needed.

This run also pinned two implementation constants: **preprocessing is `/255`
([0,1]) RGB, letterbox-padded to square then resized to 192** (the `[-1,1]`
normalization detects nothing), and the **decode params** (x/y scale 192, score
clip 100, score threshold 0.5, NMS IoU 0.3) reproduce MediaPipe detections — so
the Rust `anchors`/`palm` modules are confirmed correct on real data.

Open-question 3 (nokhwa CI build) is still open → resolved in plan Task 1.1.

## Performance follow-up & runtime-fork debate (2026-06-05)

First on-hardware run tracked, but at an unusable framerate. Root-caused with a
per-stage profiler (`pipeline::tests::profile_pipeline_stages`): on Apple
Silicon, tract ran palm in ~147 ms and landmark in ~62 ms, and palm detection
ran *every* frame → ~4.4 fps.

**In-constraint fixes shipped (no runtime swap, all pure-Rust):**

1. **Detect-then-track** — palm detection now runs only to (re)acquire; tracking
   frames derive the ROI from the previous frame's landmarks
   (`roi_from_landmarks`) and run the landmark model alone. ~4.4 → ~15 fps.
2. **tract 0.21 → 0.23** — two years of linalg-kernel work; landmark 62 → 46 ms.
   ~15 → ~20 fps. API migration in `TractInference` only; `cargo deny` stays
   clean. (Re-validate real-hand detection quality on hardware — the numerical
   fidelity gate was originally run on 0.21.)
3. **Render-rate One-Euro smoothing** (`smoothing.rs`, MediaPipe-provider-only) —
   the provider's `poll` eases the exposed pose toward the latest inference
   result every render frame, so a ~20 fps source renders as fluid ~60 fps
   *perceived* motion. The shared `HandTrackingState`/`TrackedHand` layer and the
   Leap path are untouched.
4. **Capture pinned to 640×480** — *reverted.* The `Closest(640×480 MJPEG)`
   request failed to open the macOS/AVFoundation camera (the device does not
   enumerate that exact format), so the worker exited and no hand tracked. Back
   to `AbsoluteHighestFrameRate`. A resolution cap must be derived from the
   camera's *enumerated* formats on real hardware, not requested blind.

**Runtime-fork debate (the Fork-2 `ort` fallback question, revisited under the
new perf data).** Three senior-engineer reviewers evaluated breaking past the
~15–20 fps tract CPU ceiling. Consensus: **do not swap runtimes.**

- **Native `ort` + CoreML/CPU** — constraint-fit 2/5. Would reach 30–60 fps, but
  reintroduces a CDN-fetched C++ blob, expands CVE surface `cargo audit` can't
  see, trips a strict `[sources]` allowlist, and can't serve the WebGPU web
  target. Discards the exact property tract was chosen for.
- **Pure-Rust GPU (burn-wgpu)** — constraint-fit 3/5. Preserves pure-Rust, but
  **can't share Bevy's wgpu device** (wgpu 27 vs 29) → a second GPU device →
  render contention + *more* heat (against the thermal goal), and the Linux NUC
  (primary deploy target) may see little/no win.
- **Optimize in-constraint (chosen)** — the target is 60 fps *perceived* motion,
  not 60 fps *inference*; human hand motion is ~5–10 Hz, so ~20 fps inference +
  render-rate smoothing suffices.

**When to revisit a swap:** if, after these fixes, palm re-acquisition hitch is
user-visible on the NUC even with smoothing (e.g. constant hand entry/exit at
the pi-party), *or* the NUC can't sustain ~15 fps inference. The lean is then
**pure-Rust GPU for thermal offload, not `ort`** (ort sacrifices the pure-Rust
property *and* adds CPU heat). The `HandInference` trait keeps either swap
localized.

## Feel fixes from the second hardware test (2026-06-05)

The smoothed build felt much better, but three issues remained: a stuck-on
attractor, a jittery palm that "warps," and residual jumpiness. Two were
concrete provider bugs, root-caused in code (not feel guesses) and fixed.

1. **Attractor stuck on — the depth term blew up.** Line's power model is
   `wanted = grab^1.5 · 5^((−z + 350) / 160)`, written for Leap, which reports
   depth `z` in mm `[40, 350]`. The MediaPipe pipeline passed the landmark
   model's *relative* z straight through `image_norm_to_leap_mm` — a near-zero
   value (`project_landmarks` scales it by ROI size, so ≈ ±0.1), **not** an mm
   depth. At z≈0 the depth term is `5^(350/160) ≈ 34×`, so power pinned high
   regardless of grab (and any z jitter was amplified 34×). **Fix:** pin
   `palm_position.z` to a fixed mid-range proxy
   (`coords::MEDIAPIPE_DEPTH_PROXY_MM = 120`), making the depth term a constant
   ~10× so **grab alone** drives strength. Calibrated against the known-good
   mouse reference (`MOUSE_POWER_PRESS = 10`): a full fist reaches power ≈10 ≈ a
   mouse press. A single webcam has no reliable hand-Z; a *size-based* depth
   proxy (apparent hand size → z, closer ⇒ stronger like Leap) is the future
   enhancement. Until then this constant is the one strength knob.
2. **Periodic "warp" — re-detect replaced the tracked ROI.** The phantom fix
   (periodic re-detect) made palm *authoritative*: every 500 ms it discarded the
   landmark ROIs and substituted freshly-detected palm ROIs. A palm ROI differs
   structurally from a landmark ROI (2.6× vs 2.0× expansion, centre shifted
   toward the fingers), so the crop changed twice a second and the hand popped;
   and a frame where palm momentarily missed blinked the hand out entirely.
   **Fix:** `pipeline::reconcile_redetect` — on a re-detect frame each track
   **keeps its landmark ROI** (continuity); palm only corroborates (reset),
   tolerates (`REDETECT_MISS_LIMIT = 2` consecutive misses, so the more-reliable
   landmark stage carries a real hand across an intermittent palm miss without
   blinking), or finally drops a phantom (cleared in ~1 s). Unmatched palm
   detections become new hands.

**Tunables left for hardware A/B (Madison's call — feel, not correctness):**

- `MEDIAPIPE_DEPTH_PROXY_MM` (120) — global attractor strength. Raise → stronger
  pull at a given grab.
- `DEFAULT_MIN_CUTOFF` (2.5) / `DEFAULT_BETA` (0.02) in `smoothing.rs` — lower
  `min_cutoff` smooths the residual position jitter harder (more lag when slow).
  `WAVECONDUCTOR_HAND_SMOOTHING=off` exposes the raw pose for comparison.
- `LINE_HAND_GRAB_THRESHOLD` (0.0) / the `grab_strength` open-hand zero point —
  if a *relaxed open* hand still reads a small grab (so a faint pull lingers),
  the grab curve's open reference (`signals.rs`) needs a real-hand calibration,
  or a small deadzone. Deferred: it needs the actual relaxed-open landmark
  geometry from hardware, not a synthetic guess.

## Roadmap entry

Recommend adding a slug **`mediapipe-webcam-hands`** to `docs/superpowers/roadmap.md` (a desktop sibling of Phase-3 `ios-vision-hands`; an alternative primary input to `leap-*`). I have **not** edited `roadmap.md` because it carries a pre-existing uncommitted change from before this branch — Madison should place/sequence the slug to avoid clobbering that edit.

## Open questions

1. ~~Does `tract` run both models as-is, or is one Resize-node graph surgery needed?~~ **RESOLVED (spike):** landmark runs as-is; palm needs the `Resize`→`scales` surgery (bit-exact) and has a residual edge-fidelity gate. Runtime = `tract`.
2. ~~Does `opencv/handpose_estimation_mediapipe` emit a handedness score and world landmarks?~~ **RESOLVED (spike):** yes — both. `signals.rs` reads them.
3. Does `nokhwa`'s `input-native` build cleanly on the Linux CI runner, or does the camera need sub-feature gating? (→ resolved in plan Task 1.1)
4. ~~**NEW (spike):** Does the palm detector's bilinear-Resize edge discrepancy degrade real-hand ROI localization?~~ **RESOLVED (2026-06-04):** No — on the canonical hand image tract's top detection matches onnxruntime to 0.0004 normalized (score Δ 0.003). tract confirmed, no caveat. See *Spike results*.

## Feel fixes from the third hardware test (2026-06-05)

After the second-test fixes, three issues remained: the attractor still triggered
with the hand wide open; landmarks still drifted/jumped; and framerate still felt
low ("explore GPU options — `tract-metal` before jumping to `ort`"). The first two
were root-caused in code and against the real MediaPipe framework (a research pass
on `LandmarksSmoothingCalculator` and the hand graph configs), not feel guesses.

1. **Attractor on with an open hand — grab never reached 0.** `signals::grab_strength`
   is calibrated to ideal open-hand geometry (tips one hand-scale out → 0), but a
   real relaxed hand sits slightly curled and landmark noise jitters the fingertips,
   leaving a small *positive* grab floor at rest. Now that the depth proxy makes grab
   the sole attractor driver, that floor matters: Line's decay gate is `grab > 0`
   (`LINE_HAND_GRAB_THRESHOLD = 0.0`), so any positive floor never decays and the slow
   attack EMA builds it up. **Fix:** a rest deadzone in the provider
   (`pipeline::apply_grab_deadzone`, default `grab_rest_deadzone = 0.12`): subtract
   the deadzone and rescale so `grab ≤ deadzone → 0` while a full fist still reaches 1.
   The Line sketch — and the Leap path, whose grab truly reaches 0 — are untouched.
2. **Landmarks drift/jump — the smoother didn't work the way MediaPipe's does.** Three
   structural gaps, all now closed:
   - *No object-scale normalization.* MediaPipe's `LandmarksSmoothingCalculator`
     divides landmark velocity by the hand's apparent size (`object_scale =
     (roi_w + roi_h)/2`) before the One-Euro cutoff, so smoothing strength is
     invariant to camera distance. We filtered in raw Leap-mm space, so a close hand
     (large per-frame deltas) and a far hand smoothed differently. The adaptive
     cutoff's speed term is now divided by `smoothing::object_scale` for positional
     channels (palm position, the 21 landmarks); already-normalized channels (unit
     normal, `[0, 1]` pinch/grab) use a unit scale.
   - *Wrong One-Euro regime.* `min_cutoff` was 2.5 Hz (≈21%/frame toward target at
     60 fps — barely smooths a held hand) with `beta` 0.02 (almost no velocity
     adaptivity). MediaPipe runs a *low* min_cutoff with a *high* beta. New defaults:
     `min_cutoff = 1.0` (≈9.5%/frame — much steadier at rest) and `beta = 2.0`, the
     latter now in scale-normalized hand-lengths/sec so the cutoff still opens up
     promptly during motion. The render-rate 60 fps easing Madison liked is unchanged.
   - *Track-id churn reset the filter.* `HandTracker` keyed identity on per-frame
     chirality, but MediaPipe's handedness flickers frame-to-frame; a one-frame flip
     spawned a new id, resetting that hand's smoothing bank (keyed on id) and popping
     the pose. Identity is now matched by **palm position alone**; chirality is held
     per-track and flips only after `CHIRALITY_FLIP_FRAMES = 4` consecutive
     disagreements (`assign` returns the held chirality, so downstream handedness no
     longer flickers either).

**Tunables — live in-app controls (no relaxed-open guessing, no restart).** The
three numeric feel knobs moved from env vars to **persisted `HandTrackingSettings`
fields**, edited live in the dev panel (Shift+D → *HAND TUNING (MediaPipe)*), which
also shows a live grab/pinch readout. `apply_mediapipe_tuning_settings` forwards
changes to the running provider each time they change. Env vars were the agent's
iteration lever; the operator wanted movable controls.

| Setting (default) | Dev-panel slider | Effect |
| --- | --- | --- |
| `grab_rest_deadzone` (0.12) | Grab rest deadzone (0–0.6) | A relaxed-open hand whose raw grab is ≤ this reads 0. **Open-hand calibration:** set to 0, open hand, read the grab floor in the readout, then raise just past it. |
| `smoothing_min_cutoff` (1.0) | Smoothing min cutoff, Hz (0.1–5) | Lower = steadier when still (more lag on slow motion). |
| `smoothing_beta` (2.0) | Smoothing beta (0–10) | Higher = opens the cutoff faster during motion (less lag). |

Still env/compile-time: `WAVECONDUCTOR_HAND_SMOOTHING=off` (expose the raw inference
pose for A/B); `MEDIAPIPE_DEPTH_PROXY_MM` (120, global attractor strength —
compile-time, not yet a setting).

**Why #1 still reproduced at the 0.12 default, and the calibration fix.** On the
third re-test the attractor still lingered with the hand open: a real relaxed-open
grab *floor* exceeds 0.12, so the deadzoned grab never reaches 0, Line's `grab > 0`
decay gate never fires, and the slow attack EMA leaves the attractor stuck at
whatever it built up. The constant was a guess. The live readout + slider replace
the guess with a measurement — read your floor, set the deadzone past it — and the
value persists. (A future "calibrate open hand" button could capture the floor on a
click.)

### Issue #3 — GPU acceleration: senior-engineer debate consensus

Two senior engineers (pragmatist/defer vs. forward-invest/unify) debated the runtime
options against the research (tract-metal, ort, wonnx, burn, candle). They **converged**:

- **Much of "jumpy" is not raw fps** — it is the jitter/warp/reset fixed in software
  this round. Re-measure feel *after* these land before spending on a runtime.
- **`tract-metal` accelerates only the M1 dev box.** tract's GPU is Metal-or-CUDA
  only — no Intel-iGPU / Vulkan / WebGPU — so it does nothing for the deploy fleet
  (Intel NUC, Windows-without-NVIDIA) or the WebGPU-only web target. It is a near-free
  dev nicety behind the `HandInference` trait, not a fix for the bottleneck.
- **`wonnx` is out** — the natural pure-Rust ONNX-on-wgpu unified path, but
  unmaintained since 2023-09-30; unacceptable under a multi-hour kiosk.
- **The real fork, gated on a measurement, is `ort` vs `burn`-on-wgpu:** `ort`
  (CoreML + DirectML) is the lower-effort *native* deploy win but reintroduces a C++
  blob per platform, needs a from-source OpenVINO build to actually reach the NUC's
  iGPU, and gives zero web acceleration. `burn` on CubeCL/wgpu is the only *maintained*
  single codepath across Mac/NUC/Windows/Web, but is a heavier dep with build-time
  model codegen and — the make-or-break risk — shares the GPU with Bevy's renderer,
  threatening the #1 thermal-stability goal.

**Consensus decision-gated sequence:** (1) land the feel-fixes (done); (2) add
trait-seam instrumentation — per-frame tracking-inference ms + track-churn counts;
(3) profile the **Intel NUC** under a representative multi-hour session. Then gate:
**A — Feel:** if smooth on the NUC at current fps after the fixes → **stop, no runtime
migration.** **B — NUC inference:** if median tracking-frame inference > ~33 ms or it
thermal-throttles → move the NUC to a GPU backend (OpenVINO/Vulkan) behind the trait.
**C — Windows-on-non-NVIDIA:** if needed → `ort` + DirectML, *only* on that platform.
**D — Web:** if web hand-tracking joins the roadmap → time-box a `burn`-wgpu spike with
a hard thermal-coexistence gate (≥30-min co-run with the renderer, no frame-time
regression); commit only if it passes, else `ort` native + web deferred. `tract-metal`
is optional and macOS-gated; it changes nothing about this sequence.

## GPU inference landed + the two problems it exposed (2026-06-05)

The `ort`/CoreML backend (feature `hand-tracking-mediapipe-ort`, runtime-selected,
tract default) made inference fast — `profile_inference_backends` on M1:

| stage | tract CPU | ort CoreML | speedup |
| --- | --- | --- | --- |
| `palm.run` | 299 ms | 32.5 ms | 9.2× |
| `landmark.run` | 89.5 ms | 1.25 ms | 71.5× |

With inference off the critical path, two problems the framework solves became
visible. One fix below was later found to be too confidently described as
"matching MediaPipe"; the 2026-06-07 reassessment makes it first-class follow-up
work instead of treating it as done.

1. **Tracking drift (position/rotation/scale, ~2×/sec reset).** Detect-then-track
   drift: `roi_from_landmarks` derived rotation from wrist→middle-MCP[9], a short
   single-point baseline whose angle noise compounds (rotate crop → biased
   landmarks → rotate more); the 500 ms palm re-detect snapped it back (hence the
   reset cadence == re-detect period). **Fix:** match `HandLandmarksToRectCalculator`
   — rotate so wrist → weighted-mean(landmarks 4, 6, 8) (`((L4+L8)/2 + L6)/2`, a
   long noise-robust baseline) points up, and shift the ROI centre toward the
   fingers by `TRACK_ROI_SHIFT_Y = -0.1` along the rotated axis so the palm stays
   centred. The transform stays memoryless (MediaPipe adds no ROI
   blending/hysteresis — stability is geometric).

   **2026-06-07 correction:** this parity claim was too broad. The upstream
   `HandLandmarksToRectCalculator` applies its `4, 6, 8` constants to a partial
   landmark subset, not directly to the full 21-landmark index space. The verified
   full-landmark mapping is `5, 9, 13` (index/middle/ring MCPs), and the tracking
   bounding box uses the same upstream 12-landmark subset rather than all 21
   landmarks. `landmark.rs` now mirrors that mapping and has regression tests that
   prove full-index `4, 6, 8` and excluded fingertips do not drive the tracking ROI.
2. **Multi-second lag at good fps (frame backlog).** The worker capped itself by
   *sleeping* the rest of a fixed frame budget after each frame; while it slept the
   camera buffer filled, so it processed ever-staler frames and latency grew
   unbounded. **Fix (FlowLimiter analogue):** never sleep with a retained frame.
   On a captured frame, either process immediately when the `max_hz` budget allows
   it or drop/report the fresh over-budget frame and keep draining capture. This
   preserves the newest-frame-wins invariant (≤1 in flight) while still restoring
   the thermal cap.

Possible follow-ups if anything still drifts/lags: drive re-detection by the
landmark presence score instead of a fixed 500 ms timer (MediaPipe re-detects on
score, not a clock); a true two-thread FlowLimiter (capture overwrites a 1-slot
latest-frame, processor consumes) if the single-thread pacing leaves residual lag;
and re-check whether the render-rate output smoothing still earns its latency now
that the pose is clean.

## Reassessment & recovery sequence (2026-06-07)

The branch should continue, but the work order changes. The fastest path to a
reliable provider is **not** "Python sidecar now" and not another blind runtime
swap. Fix the MediaPipe harness parity and observability first, then let NUC
telemetry decide whether the current backend stack is good enough.

**Observed implementation problems:**

1. **Tracking-ROI graph parity was wrong before Phase 11.1.** `roi_from_landmarks`
   used upstream partial-landmark indices as if they were full 21-landmark
   indices, and it bounded all landmarks instead of the graph's tracking subset.
   Phase 11.1 fixes the code and tests; live A/B against the Python/first-party
   MediaPipe oracle is still useful before acceptance.
2. **`max_inference_hz` needed drop-not-sleep semantics.** The previous sleep cap
   caused camera backlog, but removing the cap entirely left no thermal budget.
   Phase 11.3 restores the cap by dropping/reporting over-budget fresh frames
   before inference, preserving freshness while honoring the thermal limit.
3. **Diagnostics are not strong enough.** The design promised inference latency
   and dropped frames, but the worker still surfaces coarse status. It needs
   capture age, stage timings, palm-run reason, track churn, dropped-frame count,
   backend name, selected camera format, and last pipeline error.
4. **Camera format selection is uncontrolled.** `AbsoluteHighestFrameRate` can
   choose a high-res or compressed mode that wastes CPU on decode. The provider
   must enumerate supported modes and pick a bounded format that exists on the
   device rather than requesting one blind.
5. **Hot-path copies/allocations remain.** Preprocess/resize/warp/tensor wrapping
   and the `ort` backend still copy owned buffers per frame. The pipeline needs
   reusable scratch buffers; the `ort` path should use preallocated tensors or I/O
   binding where the crate supports it, otherwise document the remaining copies.
6. **`palm_velocity` is zeroed.** The pipeline passes `palm_pos` as both previous
   and current position. Velocity-dependent consumers and diagnostics cannot be
   trusted until the tracker exposes previous position.

**Recommended sequence:**

1. **Parity first:** verify landmark-to-rect against upstream MediaPipe and the
   Python oracle, then replace tests that merely lock in the current assumption.
   Code/tests for the upstream partial-landmark mapping landed in Phase 11.1;
   manual camera A/B remains.
2. **Instrumentation second:** add per-stage metrics before more tuning so every
   feel complaint can be tied to frame age, model time, track churn, or capture.
3. **Flow/capture third:** honor `max_inference_hz` with drop-not-sleep semantics
   and choose camera formats from enumeration.
4. **Allocation/copy cleanup fourth:** preallocate pipeline buffers and reduce
   `ort` input/output copies after the metrics identify the real hot spots.
5. **Hardware gate:** run the NUC with those metrics. If it is smooth and thermally
   stable, stop there. If not, add platform-native `HandInference` backends
   (OpenVINO/DirectML/CoreML) or a native MediaPipe/Tasks sidecar before choosing a
   Python sidecar. Python remains the oracle and an emergency/demo fallback.

---

## Post-review architecture revisions (2026-06-09)

This addendum records the **current** architecture on the
`mediapipe-hand-tracking` branch as of 2026-06-09. It supersedes any section
above where there is a conflict.

### 1. ort-only inference backend

`tract` has been deleted. `ort` (ONNX Runtime) with the CoreML execution
provider on macOS and a CPU fallback elsewhere is now the sole backend. The
feature flag `hand-tracking-mediapipe-ort` has been collapsed into
`hand-tracking-mediapipe` — there is no runtime-selection toggle.

### 2. Real presence gating (phantom-hand fix)

The ONNX landmark model's presence and handedness heads have Sigmoid baked into
the graph. Outputs are selected by declared index order (`LandmarkOutputs`:
index 0 image, 1 presence, 2 handedness, 3 world). The pipeline consumes raw
probabilities — no second sigmoid pass. The presence gate (threshold 0.5)
genuinely evicts tracks when the model reports no hand, fixing the phantom-hand
symptom. Handedness is live (was always Right).

### 3. Letterbox unprojection (content-rect coordinates)

Landmarks and palm centers are computed in square-normalized coordinates (the
padded square the models see), then unmapped through `ContentRect::to_content_norm`
before the `image_norm_to_leap_mm` call. On a 16:9 source, the old path
compressed Y to 56% of the Leap range (1.78× squeeze); the full Leap range is
now reachable.

### 4. World-landmark gesture signals

Grab, pinch, and palm normal are derived from the model's metric WORLD
landmarks (pose-invariant, wrist-centred metres). The previous image-landmark
derivation produced false fists with a tilted open hand; world landmarks fix
that. World landmarks remain pipeline-internal and are not exposed on `Hand`.

### 5. Size-estimated depth with k calibration

A fixed 120 mm depth pin has been superseded by pinhole-inversion depth
estimation: `distance = k * world_size / image_size`. The calibration gain `k`
(default 0.8, dev-panel slider "Depth calibration k") is live-tunable. Setting
`k = 0` restores the 120 mm pin — the instant rollback knob on stage. Estimated
distance is EMA-smoothed per-track (0.4 s time constant); association gating
uses xy only so z noise does not spawn phantom ids. A "Est. distance (mm)"
dev-panel diagnostic shows the live estimate.

### 6. Scale-relative association gate

The fixed 0.25 association gate has been replaced by `0.5 * max(sizes)` floored
at 0.08. The spread check uses `min(bbox_w, bbox_h)` instead of the mean, so a
line-collapsed landmark set is rejected. Regression tests pin both the old
defects (phantom second hand from the fixed gate; false fist from the mean
metric).

### 7. Allocation-free detect path

The hot path is free of per-iteration allocations. Decode, NMS, and resize use
reusable scratch buffers (pre-allocated at init, cleared with `vec.clear()` to
preserve capacity). Sort uses `sort_unstable`; the no-op re-sort on the tracking
path was deleted. Worker and pipeline loops are allocation-free by construction.

### 8. Runtime provider selector (2026-06-10)

Provider selection is now a persisted user setting
(`HandTrackingSettings::provider`, a `HandProviderChoice` enum: `Auto` /
`Leap` / `MediaPipe` / `Off`) rendered as a dropdown in the user settings
panel (Settings → Hand Tracking → "Tracking provider") and switchable live:
the registry is torn down (each provider `stop()`ed synchronously, so the
camera/device is released before the successor starts) and rebuilt without a
restart. The choice → fallback policy lives in `wc_core::input::selection`
(unit-tested with mock providers); the concrete constructors stay in the
binary (`crates/waveconductor/src/hand_providers.rs`) and are injected as
closures.

`Auto` now probes Leap → MediaPipe → silent mock (MediaPipe was previously
env-only). Because MediaPipe's camera opens asynchronously on the worker
thread, Auto registers it optimistically and a startup watcher
(`AutoMediaPipeWatch`) demotes to the mock iff the provider reports `Errored`
before its first `Connected` — which the worker protocol guarantees means
"the camera never opened" (the worker's first message after a successful open
is `Connected`; on open failure it reports `Errored` and exits). After the
first `Connected`, transient mid-session errors never demote: the provider
keeps its honest `Errored` LED.

`WAVECONDUCTOR_HAND_PROVIDER` is demoted to a dev/deployment pin: when set
(`auto` | `leap` | `mediapipe` | `off` | `mock` | `synthetic`) it wins over
the setting for the whole session and the live switch system disables itself.
`mock` / `synthetic` remain env-only test fixtures (not in the user-facing
enum). An unrecognized value now warns and defers to the setting (previously:
treated as `auto`).

The `hand-tracking-mediapipe` feature is now in the binary's `default`
feature set — this supersedes the "not in `default` (opt in explicitly)" note
in the feature-flags section above. Both backends ship in the deployment
binary; the runtime selector decides which one runs.
