"""Reproducibly produce the CoreML-accelerated palm-detection model.

The OpenCV-Zoo MediaPipe palm detector ships 26 `PReLU` activations whose slope
initializers are shaped `[1, C, 1, 1]`. ONNX Runtime's CoreML execution provider
rejects `PReLU` unless the slope is `[C, 1, 1]` or a scalar, and because `PReLU`
sits after every conv, those rejections fragment the conv backbone into 30
CoreML/CPU partitions (slower than plain CPU). This tool reshapes the 26 slopes
`[1, C, 1, 1] -> [C, 1, 1]`, which collapses the graph to 6 partitions and is
**bit-exact** (both shapes broadcast identically against `[N, C, H, W]`; verified
max-abs-err 0.0 vs the upstream model).

This is a dev-only tool. Its output `assets/models/hand/palm_detection.onnx` is
committed, so the build/deploy never needs Python or the network. Re-run only
when re-vendoring the upstream model.

    uv run --with onnx --with numpy --with onnxruntime tools/handtrack-oracle/graph_surgery.py

History: revisions before June 2026 also rewrote the two FPN `Resize` nodes from
the `sizes` input form to explicit `scales=[1,1,2,2]` (a workaround for the tract
runtime, which ignored `sizes`) and stripped the two initializers that rewrite
orphaned. Both edits were reverted once the runtime moved to ONNX Runtime, which
honours the upstream `sizes` form, leaving the PReLU reshape below as the model's
only modification. See `docs/runbooks/onnx-coreml-model-surgery.md`.

Upstream original (Apache-2.0):
  https://huggingface.co/opencv/palm_detection_mediapipe/resolve/main/palm_detection_mediapipe_2023feb.onnx
"""

import hashlib
import sys
import urllib.request
from pathlib import Path

import numpy as np
import onnx
from onnx import numpy_helper

ROOT = Path(__file__).resolve().parents[2]
MODELS = ROOT / "assets" / "models" / "hand"
ORIG_URL = (
    "https://huggingface.co/opencv/palm_detection_mediapipe/resolve/main/"
    "palm_detection_mediapipe_2023feb.onnx"
)
ORIG_SHA256 = "78ff51c38496b7fc8b8ebdb6cc8c1abb02fa6c38427c6848254cdaba57fcce7c"


def reshape_prelu_slopes(model: onnx.ModelProto) -> int:
    """Reshape every `[1, C, 1, 1]` PReLU slope initializer to `[C, 1, 1]`.

    Bit-exact: the two shapes broadcast identically against the `[N, C, H, W]`
    activation. Returns the number of slopes reshaped.
    """
    g = model.graph
    inits = {i.name: i for i in g.initializer}
    n = 0
    for node in g.node:
        if node.op_type != "PRelu":
            continue
        slope = inits.get(node.input[1])
        if slope is not None and len(slope.dims) == 4 and slope.dims[0] == 1:
            arr = numpy_helper.to_array(slope)
            slope.CopyFrom(numpy_helper.from_array(arr.reshape(arr.shape[1:]), name=node.input[1]))
            n += 1
    return n


def main() -> int:
    src = MODELS / "palm_detection_original.onnx"
    if not src.exists():
        print(f"downloading upstream palm detector → {src}")
        MODELS.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(ORIG_URL, src)

    digest = hashlib.sha256(src.read_bytes()).hexdigest()
    if digest != ORIG_SHA256:
        print(f"ERROR: upstream SHA-256 mismatch\n  got      {digest}\n  expected {ORIG_SHA256}")
        return 1

    model = onnx.load(str(src))
    n = reshape_prelu_slopes(model)
    onnx.checker.check_model(model)
    out = MODELS / "palm_detection.onnx"
    onnx.save(model, str(out))
    print(f"reshaped {n} PReLU slopes → {out}")

    # Verify bit-exactness vs the upstream original under onnxruntime, if available.
    try:
        import onnxruntime as ort

        x = np.random.default_rng(1234).standard_normal((1, 192, 192, 3)).astype(np.float32)
        a = ort.InferenceSession(str(src), providers=["CPUExecutionProvider"]).run(None, {"input_1": x})
        b = ort.InferenceSession(str(out), providers=["CPUExecutionProvider"]).run(None, {"input_1": x})
        err = max(float(np.max(np.abs(p - q))) for p, q in zip(a, b))
        print(f"onnxruntime upstream-vs-reshaped max-abs-err = {err:.3e} (expect 0.0)")
    except ImportError:
        print("onnxruntime not present; skipped bit-exactness check")
    return 0


if __name__ == "__main__":
    sys.exit(main())
