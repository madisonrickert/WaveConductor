# handtrack-oracle (dev-only)

Local-only Python helpers for the MediaPipe webcam hand-tracking provider. **Not
shipped, not a build/CI runtime dependency, no API spend.** Managed with `uv`.

These exist to (a) decide and de-risk the Rust ONNX runtime (the *verification
spike*), and (b) regenerate the vendored model assets when the upstream models
are re-vendored.

## Scripts

### `graph_surgery.py` — produce the CoreML-accelerated palm model
Downloads the OpenCV-Zoo MediaPipe palm detector and reshapes its 26 `PReLU`
slope initializers `[1,C,1,1]` → `[C,1,1]` so ONNX Runtime's CoreML EP accepts
them (palm graph 30 → 6 partitions) — **bit-exact under onnxruntime**. Writes
`assets/models/hand/palm_detection.onnx` (committed, shipped). (Earlier this tool
did a `tract`-era `Resize` `sizes`→`scales` rewrite, since reverted; see
`docs/runbooks/onnx-coreml-model-surgery.md`.)

```bash
uv run --with onnx --with numpy --with onnxruntime tools/handtrack-oracle/graph_surgery.py
```

### `spike_io.py` — onnxruntime reference for the tract diff
Loads both vendored models, runs onnxruntime on a deterministic seeded random
input, and dumps every input/output tensor to `tests/fixtures/hand/*.npy` plus a
`spike_manifest.json` of tensor names/shapes/dtypes. The Rust spike loads the
same models in tract and diffs against these.

```bash
uv run --with onnxruntime --with numpy tools/handtrack-oracle/spike_io.py
```

## Spike findings (2026-06-04)

See the **Spike Results** section of
`docs/superpowers/specs/2026-06-04-mediapipe-webcam-hand-tracking-design.md` for
the recorded decision. Summary:

- `hand_landmark.onnx`: tract matches onnxruntime to ~1e-4 (incl. handedness +
  world-landmark outputs). No changes needed.
- `palm_detection.onnx`: needs the `Resize` graph surgery (above) to run in
  tract at all; after surgery the shapes are correct but the FPN `linear`/
  `half_pixel` Resize diverges from onnxruntime at feature-map **edges** (tract
  extrapolates; onnxruntime clamps). Open gate: validate palm-ROI accuracy on a
  real hand image before final commitment; mitigation ladder documented in the
  spec.

## Real-hand fixtures (later)
The end-to-end golden test (plan Phase 6.2) needs a real hand image
(`tests/fixtures/hand/sample_hand.png`) and oracle-generated golden landmarks.
The seeded-random `.npy` files above are spike evidence only and are not
committed (regenerate with `spike_io.py`).
