# Runbook: ONNX model surgery for CoreML acceleration

**Use this when** a vendored `.onnx` model logs a high CoreML partition count at
startup, runs slower than expected on Apple Silicon, or you need to edit a
model's graph while keeping its output bit-identical (e.g. the next time we
vendor or re-export a hand/inference model).

Written from two surgeries on `assets/models/hand/palm_detection.onnx`
(June 2026, commits `16dd90f` and `d2369f4f`). The inference backend is
[`crates/wc-core/src/input/providers/mediapipe/inference_ort.rs`](../../crates/wc-core/src/input/providers/mediapipe/inference_ort.rs);
the models are vendored under `assets/models/hand/`.

---

## TL;DR (read before touching a model)

1. **The CoreML cache will lie to you.** Before testing *any* model change, clear it:
   ```bash
   rm -rf ~/Library/Caches/waveconductor/coreml-cache
   ```
   ONNX Runtime's CoreML EP keys its compiled-artifact cache by a hash that does
   **not** change when you only edit initializers, so a stale artifact for the
   *old* graph gets served for your *new* graph and crashes at inference with
   `output_features has no value`. The code now namespaces the cache by a hash of
   the model bytes (`model_cache_key`), but clear it anyway while iterating.
2. **Diagnose with Python `onnxruntime`** (read-only, uses no cache): capture the
   verbose CoreML capability log to see exactly which nodes fall to CPU and *why*.
3. **Edit with `onnx`** (Python): the useful edits are metadata-only and
   **bit-exact**. Always verify with a CPU-EP numerical diff of `0.0`.
4. **Verify in the real Rust runtime.** Python/PyPI `onnxruntime` is **not** the
   same binary as the `ort` crate's pyke build, and (critically) Python uses no
   cache. Only `cargo nextest run -p wc-core --features hand-tracking-mediapipe`
   tells the truth. Two separate days were lost to trusting the Python proxy.

---

## Tooling

No project venv. Everything runs through `uv` with inline deps:

- **Inspect / edit a graph:** `uv run --with onnx --with numpy python - <<'PY' ...`
- **Diagnose CoreML placement / partitions:** `uv run --with onnxruntime --with numpy python - <<'PY' ...`

The `onnxruntime` PyPI wheel bundles the CoreML EP on macOS, so it is a fast,
read-only proxy for *which nodes CoreML accepts and how many partitions result*
(a compile-time `GetCapability` question). It is **not** a proxy for runtime
behaviour — see the cache gotcha below.

---

## Provenance and surgery history

The vendored `palm_detection.onnx` derives from Google MediaPipe and today carries
exactly **one** modification (the PReLU reshape, Surgery 2). Two earlier edits
(Surgeries 0 and 1) were applied for the `tract` runtime and then **reverted** once
the project settled on ONNX Runtime; they are kept in this history so the model's
shape reads straight and the tract workaround is not reintroduced by reflex.
Reproduce the vendored model with `tools/handtrack-oracle/graph_surgery.py`, and
record any new edit + SHA in `assets/models/hand/ATTRIBUTION.md`.

**Lineage at a glance:**

1. **Google MediaPipe Hands — BlazePalm (`.tflite`).** google-ai-edge/mediapipe,
   Apache-2.0. 192×192 input, SSD-style anchors. Anchor decode + NMS live in
   MediaPipe's runtime, not in the model graph.
2. **OpenCV Zoo — TFLite→ONNX conversion.** `opencv/palm_detection_mediapipe`
   (`palm_detection_mediapipe_2023feb.onnx`, HuggingFace, Apache-2.0, SHA
   `78ff51c3…`). The TensorFlow-style fused node names we still read (e.g.
   `…/FusedBatchNormV3;…/depthwise_conv2d_3/depthwise;…/conv2d/Conv2D1__60`) are
   fingerprints of this step. It emits raw `[1,2016,18]` box/keypoint regressions
   + `[1,2016,1]` scores; anchor decode + NMS are left to the consumer (done in
   Rust here, so the graph carries no decode/NMS tail). **This is the upstream the
   tool downloads and surgeries from.**
3. **Surgery 0 — Resize `sizes`→`scales`** (commit `cb69ecf4`) — *reverted*.
4. **Runtime switch: tract → ort (ONNX Runtime).**
5. **Surgery 1 — strip 2 unused initializers** (commit `16dd90f`) — *reverted with Surgery 0*.
6. **Surgery 2 — reshape 26 PReLU slopes + cache fixes** (commit `d2369f4f`) — the only edit in the current model.

`hand_landmark.onnx` (the second stage) is the OpenCV-Zoo
`handpose_estimation_mediapipe` ONNX, vendored **as-is, no surgery** — tract
matched onnxruntime to ~1e-4 on it in the spike, and it has no PReLU, so CoreML
takes it cleanly.

### Surgery 0 — Resize `sizes`→`scales`, for tract (commit `cb69ecf4`) — reverted

The OpenCV-Zoo ONNX expresses its two FPN upsamples as `Resize` nodes that pass
the target size via the **`sizes`** input (a static constant — `Concat__234:0` /
`Concat__263:0`). The Phase-0 runtime, **tract 0.21**, does not honour `sizes` and
left the feature map un-resized, failing at `Resize__235`. The fix rewrote both
nodes to an explicit `scales=[1,1,2,2]` (clean 2× NCHW upsample), bit-exact under
onnxruntime.

**Reverted** once the runtime moved to ort, which honours the upstream `sizes`
form. Before reverting it was confirmed both **bit-exact** *and*
**CoreML-partition-neutral**: 6 partitions either way, because the CoreML blocker
on those nodes is the untouched `half_pixel` `coordinate_transformation_mode`, not
the `sizes`/`scales` form (see the floor section). With nothing to gain on CoreML
and `sizes` being the more upstream-faithful artifact, the workaround was dropped
once tract was gone.

### Runtime switch: tract → ort

Phase 0 chose tract, with a recorded open gate: tract's `half_pixel` Resize
extrapolates at feature-map edges where onnxruntime clamps (a real-hand ROI
accuracy risk). The provider later moved to **ort / ONNX Runtime** with the
CoreML EP (the "ort-only" mediapipe merge; backend in `inference_ort.rs`). That
switch dissolved the tract-era Resize concern (we *are* onnxruntime now) and is
what made CoreML acceleration — and therefore the PReLU reshape — relevant.

### Surgery 1 — strip 2 unused initializers (commit `16dd90f`) — reverted

Surgery 0's rewrite *dropped* the `Resize` `sizes` inputs, which orphaned the two
constants that had fed them (`Concat__234:0`, `Concat__263:0`); ORT then logged
two `Removing initializer … not used by any node` warnings at load. Stripping the
orphans offline silenced the warnings (bit-exact). This surgery existed **only** as
cleanup for Surgery 0, so reverting Surgery 0 dissolved it too: with the upstream
`sizes` form restored, those two initializers are live again and there is nothing
to strip.

### Surgery 2 — reshape 26 PReLU slopes + cache fixes (commit `d2369f4f`)

Symptom: `partitions supported by CoreML: 30 ... nodes supported: 91` (of 124).
CoreML's NeuralNetwork EP rejected all 26 `PReLU` activations because their slope
initializers were shaped `[1, C, 1, 1]`; the EP requires `[C, 1, 1]` or a scalar.
`PReLU` sits after every conv, so the rejections shattered the conv backbone into
30 CoreML/CPU islands — fragmented enough that CoreML was *slower than plain CPU*.

Fix: reshape the 26 slopes `[1, C, 1, 1] → [C, 1, 1]`. Both shapes broadcast
identically against `[N, C, H, W]`, so it is **bit-exact** (verified diff `0.0`).
Result: 30 → 6 partitions, 91 → 117 CoreML nodes. This change also forced the two
companion fixes shipped in the same commit:

- **Per-model cache key** (`model_cache_key`): the production-safety fix for the
  stale-cache crash (see below). Without it, a deployed install crashes on the
  next model update.
- **`cfg(test)` disables the on-disk cache**: the unit tests load the same model
  from many parallel nextest processes, and ORT's CoreML cache-population is not
  concurrency-safe (`an item with the same name already exists`). Production
  loads each model once, so it keeps the cache.

---

## How to diagnose CoreML fragmentation

Capture ORT's verbose capability log (severity 0) while building a CoreML
session, and read which ops it rejects and why. ORT writes this at the C level
(fd 2), so redirect the OS fd, not Python's `sys.stderr`:

```python
import onnxruntime as ort, os, sys, tempfile, contextlib, re
@contextlib.contextmanager
def cap():
    tf = tempfile.NamedTemporaryFile(mode="w+", delete=False, suffix=".log")
    saved = os.dup(2); sys.stderr.flush(); os.dup2(tf.fileno(), 2)
    try: yield tf.name
    finally: sys.stderr.flush(); os.dup2(saved, 2); os.close(saved); tf.close()

so = ort.SessionOptions(); so.log_severity_level = 0      # 0 = VERBOSE
with cap() as log:
    ort.InferenceSession("model.onnx", sess_options=so,
        providers=[("CoreMLExecutionProvider", {"ModelFormat": "NeuralNetwork"}),
                   "CPUExecutionProvider"])
t = open(log, errors="replace").read()
print(re.search(r"GetCapability.*partitions.*", t).group(0))           # partition count
# per-op rejection reasons:
for ln in sorted(set(re.findall(r"IsOpSupportedImpl\] .*|IsPReluOpSupported\] .*", t))):
    print(ln)
```

The same Rust runtime dump is available via
`ORT_LOG=verbose RUST_LOG=ort=trace` (see `inference_ort::backend`), but the
Python path is faster to iterate on.

To count the *structure* before diagnosing, an op histogram + initializer-shape
dump (`onnx.load`, iterate `graph.node` / `graph.initializer`) tells you whether
the fragmenters are activations (PReLU), shape glue, or resampling.

---

## CoreML NeuralNetwork EP op-support constraints we have hit

| Op | Constraint the EP enforces | Fixable bit-exact? |
|----|----------------------------|--------------------|
| `PReLU` | slope must be `[C, 1, 1]` or a scalar | **Yes** — reshape slope `[1,C,1,1] → [C,1,1]` (done) |
| `Pad` (constant) | needs explicit `constant_value` input **and** may only pad the last two (spatial) dims | **No** — `channel_padding` ops pad the channel dim; not supported regardless of `constant_value` |
| `Resize` | only `asymmetric` `coordinate_transformation_mode` | **No** — these use `half_pixel`; switching modes changes the upsample numerics (accuracy risk) |
| `Concat` | only 4-D inputs | **No** — these are 3-D output-head concats; would need reshape gymnastics or a Rust offload |

`MLProgram` format is **not** the answer: it covers a few more ops in principle
but fails to compile these graphs (`Required param 'pad' is missing` on a fused
`Conv`, not MaxPool), and even patched it only reaches 27 partitions — worse than
NeuralNetwork's 6. Stay on NeuralNetwork.

---

## The 6-partition floor: remaining unsupported nodes

After Surgery 2, the residual 6 partitions come from **7 nodes** CoreML still
will not take, and **none are safely fixable**:

- **3 × `Pad`** (`channel_padding`): pad the channel dimension. The first log line
  blames a missing `constant_value`, but adding it (bit-exact) does **not** help —
  the real blocker is `Only padding on the last two dimensions is supported`.
- **2 × `Resize`** (`half_pixel`): the EP wants `asymmetric`. Changing the mode is
  the only "fix" and it alters the feature-pyramid upsampling, so it is off-limits
  without an accuracy re-validation of the detector.
- **2 × `Concat`** (3-D): the EP only does 4-D concat. These sit at the output
  heads (already partly offloaded to Rust); merging them would need 4-D reshape
  wrapping or moving more of the tail into Rust for marginal gain.

So 6 partitions is the practical floor for this model under NeuralNetwork without
changing what it computes. Inference is already well under the per-frame budget
and CoreML beats CPU at 6 partitions, so chasing the last few is not worth it.

---

## Evaluated alternatives (measured, not adopted)

Several "optimize the ONNX" tools were tried and measured against the CoreML
partition count. None beat the targeted PReLU reshape:

- **`onnxsim` (onnx-simplifier):** no-op here. It removes graph *redundancy*
  (constant-folds shapes, prunes dead nodes), but these converted models have
  none, and it does not touch op attributes. Measured: upstream stays 30
  partitions, our model stays 6, PReLU slope shape unchanged. It cannot make the
  EP-compatibility edit we need.
- **Re-deriving from Google's `.tflite` with modern `tf2onnx` (opset 17):**
  *worse.* The fresh conversion produced 144 nodes / **35 partitions** (vs the
  OpenCV-Zoo upstream's 124 / 30), still emitted `[1,C,1,1]` PReLU slopes (so it
  would need the same reshape), and hit the identical Pad/Resize/Concat floor.
  The 2023 OpenCV-Zoo conversion is the cleaner one.
- **Transformer optimizer** (`onnxruntime/tools/transformers`): N/A — it fuses
  attention/LayerNorm/GELU, which BlazePalm (a CNN) does not contain.
- **A newer model:** none exists. The Hand Landmarker still ships the Feb-2023
  BlazePalm + 21-landmark bundle (architecture from the 2020 paper); Google
  repackaged it (Tasks API) without retraining.

The throughline: the residual floor (channel-`Pad`, `half_pixel`-`Resize`, 3-D
`Concat`) is **model-inherent** — real operations any faithful converter must
preserve — so no conversion or simplification tool removes it without changing
what the model computes. The only converter-free variable is the PReLU slope
*shape*, which our reshape already handles.

Runtime-side, `ort` I/O binding buys nothing extra on Apple Silicon (unified
memory: no host/device copy to eliminate), so the per-frame copies were removed
directly instead: the input is a borrowed `TensorRef::from_array_view` over the
pipeline's reused buffer, and `HandInference::run` writes its outputs into a
caller-owned `Vec<Tensor>` reused across frames (grown once, refilled in place).
Steady-state inference now allocates nothing after warmup; the post-processing
path (anchor decode, NMS, ROI, smoothing) was already zero-alloc by design.

## Bit-exact edit + verify recipe

The pattern for every safe edit: change metadata only, then prove the output did
not move.

```python
import onnx, numpy as np, onnxruntime as ort
from onnx import numpy_helper
orig = open(PATH, "rb").read()
m = onnx.load(PATH)
# ... mutate m.graph (reshape an initializer, strip an unused one, etc.) ...
onnx.checker.check_model(m)
new = m.SerializeToString()

def run_cpu(b):                                   # CPU EP, no cache, deterministic
    so = ort.SessionOptions(); so.log_severity_level = 3
    s = ort.InferenceSession(b, sess_options=so, providers=["CPUExecutionProvider"])
    x = np.random.default_rng(0).standard_normal(INPUT_SHAPE).astype(np.float32)
    return s.run(None, {s.get_inputs()[0].name: x})
diff = max(float(np.abs(a-b).max()) for a, b in zip(run_cpu(orig), run_cpu(new)))
assert diff == 0.0, f"NOT bit-exact: {diff}"      # refuse to write unless identical
open(PATH, "wb").write(new)
```

A reshape that only drops/adds size-1 dims, or removing a genuinely unused
initializer, will read `0.0`. Anything non-zero means you changed the computation
— stop and reconsider.

---

## Pitfalls / false leads (do not repeat these)

- **The stale cache cost two days.** The PReLU-reshaped model crashed with
  `output_features has no value`. It was misdiagnosed first as an `ort` *version*
  problem (rc.10), then as "pyke's prebuilt binary differs from the PyPI wheel".
  Both were wrong and both were confounded by the on-disk CoreML cache: the old
  30-partition compiled artifact was being served for the new 6-partition graph.
  Python "passed" only because it set no cache dir. **Clear the cache and control
  for it before blaming the runtime.**
- **Python/PyPI `onnxruntime` ≠ the `ort` crate's binary.** Same version number
  (1.24.2), different build (pyke compiles with `--client_package_build`). Use
  Python for *capability/partition* diagnosis; confirm *runtime behaviour* only in
  the Rust test suite.
- **Read past the first rejection line.** ORT logs the first failed check; a node
  can have a deeper blocker (the `Pad` `constant_value` vs channel-dim story).
- **Don't enable `debug-assertions` on release/soak** to get the verbose dump —
  use `RUST_LOG=ort=trace` instead (the capture harness toggles are debug-only).

---

## References

- Backend + cache code: `inference_ort.rs` (`model_cache_key`, `coreml_cache_dir`,
  the `load` doc comment on NeuralNetwork vs MLProgram).
- Re-vendoring tool (applies the PReLU reshape; the asset's source of truth):
  `tools/handtrack-oracle/graph_surgery.py` (+ `README.md`, `spike_io.py`).
- Phase-0 spike + tract decision: `docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md`.
- Commits: `cb69ecf4` (vendor + Resize surgery, tract), `16dd90f` (strip),
  `d2369f4f` (PReLU reshape + cache fixes + ort rc.12); Surgeries 0 and 1 later
  reverted by re-deriving the model from upstream + the PReLU reshape only.
- Model provenance + SHAs: `assets/models/hand/ATTRIBUTION.md`.
