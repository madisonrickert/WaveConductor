# WaveConductor v5 — MediaPipe Webcam Hand-Tracking Provider

**Date:** 2026-06-04
**Workstream:** New roadmap item — proposed slug `mediapipe-webcam-hands` (see *Roadmap entry* below)
**Status:** Design — pending Madison review before plan-writing
**Scope window:** ~6–9 focused days (provider plumbing + two-stage ONNX glue + verification spike + diagnostics)
**Branch:** `mediapipe-hand-tracking` (off `v5-alpha`; merges back to `v5-alpha` on Madison's sign-off)

## Goal

Add a second real hand-tracking provider that derives 21-landmark hands from a **conventional USB webcam** using Google's MediaPipe hand models, running **fully in-process in native Rust** — no Leap Motion device, no external process, no Python at runtime. The webcam provider slots into the existing `HandTrackingProvider` seam as `ProviderId::MediaPipe`, emits frames in the **same coordinate convention the Leap provider uses**, and therefore drives every existing sketch (Line attractors, HandMesh, gesture detection, pointer merge) with **zero changes to consumer code**.

This unlocks hand-tracking on any machine with a webcam — broadening the kiosk's input options beyond the Leap Motion Controller, and serving as the desktop sibling of the roadmap's Phase-3 `ios-vision-hands` (camera-based hands on iPad). It is also a deliberate skills/portfolio investment: WaveConductor owns the full inference path — the two-stage palm→ROI→landmark pre/post-processing glue and the ONNX runtime integration — rather than calling a turnkey black box.

## Background: architecture decision record

Two architectural forks were resolved by senior-engineer subagent debate to consensus (per the project goal directive), with Madison concurring on the first.

### Fork 1 — Integration strategy (decided: **in-process Rust**)

| Path | What | Verdict |
|------|------|---------|
| **A. In-process Rust** | `nokhwa` webcam capture + an ONNX runtime running the converted MediaPipe models, glue in Rust. Single self-contained binary. | **CHOSEN.** Matches the in-process Leap house style, one PID to supervise for the multi-hour unattended soak, the existing `ProviderRegistry` + Mock-fallback acts as supervisor, and Madison explicitly wants to work closer to the ML models. |
| **B. Python MediaPipe sidecar** | A `uv`-managed Python process runs Google's first-party `HandLandmarker` and streams landmarks over the existing `websocket.rs` provider. | Rejected. Its only decisive edges were speed-to-demo (no hard deadline exists) and zero porting risk (neutralized by the numeric oracle below). Adds a permanent second process + Python runtime + systemd unit to an otherwise single-binary deploy. The Path-B advocate itself concluded "I recommend Path A." Retained **only as a dev-time validation oracle** (see below). |
| **C. WasmEdge `mediapipe-rs`** | The maintained `WasmEdge/mediapipe-rs` crate (full two-stage hand pipeline). | Rejected for native embedding. The crate runs **only** as `wasm32-wasi` inside the WasmEdge VM (maintainer-confirmed; the `wasi-nn` crate panics on native targets). Using it means embedding a whole WASM runtime + WASI-NN plugin + two TFLite shared libs beside the binary and copying every camera frame across the WASM boundary — the opposite of a single clean binary. Its excellent, complete glue is instead used as a **porting reference** for our Rust implementation. |

### Fork 2 — ONNX runtime (decided: **`tract`-first, `ort` fallback, behind a thin trait, gated by a verification spike**)

Both runtime advocates independently converged on the same resolution:

- **Shared finding that de-risks `tract`:** both ONNX graphs (`opencv/palm_detection_mediapipe`, `opencv/handpose_estimation_mediapipe`) output **raw tensors** — anchor decode and NMS happen in external Rust glue we write regardless, **not inside the graph**. The operators that usually defeat pure-Rust runtimes (`NonMaxSuppression`, `TopK`) are therefore **absent**. `tract`'s only real exposure is the palm detector's FPN `Resize`/upsample node (nearest-neighbor 2×, which `tract` supports).
- **Decision:** use **`tract`** (pure-Rust, no native C++ blob, `cargo deny`-trivial, statically linked → true single binary on all three desktop OSes, strongest "own the whole stack" learning story) as the primary runtime, **behind a thin `HandInference` trait**, with **`ort` (ONNX Runtime) as a documented fallback** if `tract` cannot run the Resize node economically. The trait makes the fallback a localized swap, not a rewrite.
- **Gate:** a **day-one verification spike** (see *Verification spike* §) loads both models in `tract`, runs a forward pass on a fixture frame, and diffs against a Python `onnxruntime` oracle within `1e-3`. If `tract` passes, it is the runtime. If the lone Resize node fails irreparably and graph surgery is uneconomical, switch the trait's default impl to `ort`. **This decision is empirical, not assumed.**

Why `tract` is favored for *this* project specifically: the roadmap makes **macOS, Linux, and Windows all first-class desktop targets**. `ort` would require vendoring and rpath-wiring a native `libonnxruntime` per platform (three blobs, a `cargo deny` SOURCES/license gap closed by a manual NOTICE, and an rc-version pin). `tract` is one pure-Rust dependency on all three. Combined with the "Hand-Z is not required" roadmap ruling (2026-05-30) — which removes any pressure for hardware-accelerated high-precision depth — the pure-Rust CPU path is comfortably sufficient for two ~2–5 MB models at a capped inference rate.

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
 │ Nokhwa     │──▶│ palm      │──▶│ ROI crop +   │──▶│ landmark  │   │   │ MediaPipeProvider     │
 │ FrameSource│   │ detect    │   │ rotate (affine)│  │ regress   │   │   │   ::poll() drains ring│
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

## Roadmap entry

Recommend adding a slug **`mediapipe-webcam-hands`** to `docs/superpowers/roadmap.md` (a desktop sibling of Phase-3 `ios-vision-hands`; an alternative primary input to `leap-*`). I have **not** edited `roadmap.md` because it carries a pre-existing uncommitted change from before this branch — Madison should place/sequence the slug to avoid clobbering that edit.

## Open questions (resolved during the spike, recorded in the plan)

1. Does `tract` run both models as-is, or is one Resize-node graph surgery needed? (→ runtime decision)
2. Does `opencv/handpose_estimation_mediapipe` emit a handedness score and world landmarks, or must chirality/3D be derived from image landmarks? (→ `signals.rs` design)
3. Does `nokhwa`'s `input-native` build cleanly on the Linux CI runner, or does the camera need sub-feature gating? (→ feature-flag shape)
