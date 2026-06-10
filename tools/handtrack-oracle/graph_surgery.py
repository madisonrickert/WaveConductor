"""Reproducibly produce the tract-compatible palm-detection model.

The OpenCV-Zoo MediaPipe palm detector expresses its two FPN upsamples as
`Resize` nodes that pass the target size via the **`sizes`** input with empty
`scales`. tract (0.21) does not honour the `sizes` input and leaves the feature
map un-resized, so the graph fails at `Resize__235`. We rewrite both `Resize`
nodes to use an explicit `scales=[1,1,2,2]` (a clean 2x NCHW upsample), which is
**bit-exact** under onnxruntime (verified: max-abs-err 0.0 vs the original) and
which tract executes with correct shapes.

This is a dev-only tool. Its output `assets/models/hand/palm_detection.onnx` is
committed, so the build/deploy never needs Python or the network. Re-run only
when re-vendoring the upstream model.

    uv run --with onnx --with numpy --with onnxruntime tools/handtrack-oracle/graph_surgery.py

Upstream original (Apache-2.0):
  https://huggingface.co/opencv/palm_detection_mediapipe/resolve/main/palm_detection_mediapipe_2023feb.onnx
"""

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


def rewrite_resizes(model: onnx.ModelProto) -> int:
    g = model.graph
    scales_name = "wc_resize_scales_2x"
    if not any(i.name == scales_name for i in g.initializer):
        g.initializer.append(
            numpy_helper.from_array(
                np.array([1.0, 1.0, 2.0, 2.0], dtype=np.float32), name=scales_name
            )
        )
    n = 0
    for node in g.node:
        if node.op_type == "Resize":
            data = node.input[0]
            roi = node.input[1] if len(node.input) > 1 else ""
            del node.input[:]
            node.input.extend([data, roi, scales_name])  # drop the `sizes` input
            n += 1
    return n


def main() -> int:
    src = MODELS / "palm_detection_original.onnx"
    if not src.exists():
        print(f"downloading upstream palm detector → {src}")
        MODELS.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(ORIG_URL, src)
    model = onnx.load(str(src))
    n = rewrite_resizes(model)
    onnx.checker.check_model(model)
    out = MODELS / "palm_detection.onnx"
    onnx.save(model, str(out))
    print(f"rewrote {n} Resize nodes → {out}")

    # Verify bit-exactness vs the original under onnxruntime, if available.
    try:
        import onnxruntime as ort

        x = np.random.default_rng(1234).standard_normal((1, 192, 192, 3)).astype(np.float32)
        a = ort.InferenceSession(str(src), providers=["CPUExecutionProvider"]).run(None, {"input_1": x})
        b = ort.InferenceSession(str(out), providers=["CPUExecutionProvider"]).run(None, {"input_1": x})
        err = max(float(np.max(np.abs(p - q))) for p, q in zip(a, b))
        print(f"onnxruntime original-vs-surgeried max-abs-err = {err:.3e} (expect 0.0)")
    except ImportError:
        print("onnxruntime not present; skipped bit-exactness check")
    return 0


if __name__ == "__main__":
    sys.exit(main())
