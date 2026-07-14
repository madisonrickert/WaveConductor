"""Reproducibly fetch and prepare the vendored BlazePose models.

Downloads the OpenCV-Zoo MediaPipe person-detection and pose-estimation ONNX
models, verifies their graph I/O contracts, and normalizes the detector's input
interface to the family standard so the Rust pipeline needs exactly one
preprocessing convention.

Why the detector needs surgery: the zoo's person-detection export takes NCHW
`[1,3,224,224]` in `[-1,1]` (its demo preprocesses `(x/255 - 0.5) * 2` and
transposes HWC->CHW), while every other vendored model in this repo — palm,
hand landmark, pose landmark — takes NHWC in `[0,1]`, which is what
`fill_nhwc_unit` in `wc-core`'s body pipeline produces. This tool prepends the
demo's own preprocessing inside the graph (`Sub(0.5)` -> `Mul(2)` ->
`Transpose(0,3,1,2)`) and re-declares the input as `[1,224,224,3]`, so the
vendored file presents the family contract. The edit is **bit-exact**: the
prepended ops are single-rounding fp32 elementwise/data-movement ops, and the
script refuses to write unless `new(x_nhwc01)` equals
`orig(transpose((x-0.5)*2))` with max-abs-err exactly 0.0 on the CPU EP.

The pose-landmark model already matches the family contract (NHWC `[0,1]`, per
the zoo's `mp_pose.py` demo) and is vendored as-is.

This is a dev-only tool. Its outputs under `assets/models/pose/` are committed,
so the build/deploy never needs Python or the network. Re-run only when
re-vendoring the upstream models (e.g. swapping in the explicit `full` variant
from the PINTO model zoo if hardware testing wants a heavier landmark model —
re-check that variant's input layout/range before reusing the surgery as-is).

    uv run --with onnx --with numpy --with onnxruntime tools/pose-models/fetch_and_prepare.py

After running, refresh the SHA-256 lines in `assets/models/pose/ATTRIBUTION.md`
from this script's output, and re-run the model contract tests:
`cargo nextest run -p wc-core --features body-tracking-mediapipe model_tests`.
See `docs/runbooks/onnx-coreml-model-surgery.md` for the partition-diagnosis
procedure that accompanies any re-vendoring.
"""

import hashlib
import sys
import urllib.request
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
from onnx import TensorProto, helper

REPO = Path(__file__).resolve().parents[2]
POSE_DIR = REPO / "assets" / "models" / "pose"

DETECTOR_URL = (
    "https://huggingface.co/opencv/person_detection_mediapipe/resolve/main/"
    "person_detection_mediapipe_2023mar.onnx"
)
LANDMARK_URL = (
    "https://huggingface.co/opencv/pose_estimation_mediapipe/resolve/main/"
    "pose_estimation_mediapipe_2023mar.onnx"
)

DETECTOR_PATH = POSE_DIR / "pose_detection.onnx"
LANDMARK_PATH = POSE_DIR / "pose_landmark_full.onnx"


def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def download(url: str) -> bytes:
    print(f"fetching {url}")
    with urllib.request.urlopen(url) as r:
        b = r.read()
    print(f"  {len(b)} bytes, sha256 {sha256(b)}")
    return b


def io_shapes(model: onnx.ModelProto):
    def dims(vi):
        return [d.dim_value for d in vi.type.tensor_type.shape.dim]

    g = model.graph
    return [dims(i) for i in g.input], sorted(dims(o) for o in g.output)


def surgery_nhwc01_input(detector_bytes: bytes) -> bytes:
    """Prepend (x - 0.5) * 2 + HWC->CHW transpose; re-declare input as NHWC."""
    m = onnx.load_from_string(detector_bytes)
    g = m.graph
    assert len(g.input) == 1, [i.name for i in g.input]
    old_name = g.input[0].name

    new_in = helper.make_tensor_value_info("input_nhwc01", TensorProto.FLOAT, [1, 224, 224, 3])
    g.initializer.extend(
        [
            helper.make_tensor("wc_pre_half", TensorProto.FLOAT, [], [0.5]),
            helper.make_tensor("wc_pre_two", TensorProto.FLOAT, [], [2.0]),
        ]
    )
    nodes = [
        helper.make_node("Sub", ["input_nhwc01", "wc_pre_half"], ["wc_pre_centered"], name="wc_pre_sub"),
        helper.make_node("Mul", ["wc_pre_centered", "wc_pre_two"], ["wc_pre_scaled"], name="wc_pre_mul"),
        helper.make_node(
            "Transpose", ["wc_pre_scaled"], [old_name], perm=[0, 3, 1, 2], name="wc_pre_transpose"
        ),
    ]
    del g.input[:]
    g.input.extend([new_in])
    for node in reversed(nodes):
        g.node.insert(0, node)
    onnx.checker.check_model(m)
    return m.SerializeToString()


def verify_bit_exact(orig_bytes: bytes, new_bytes: bytes) -> None:
    def session(b):
        so = ort.SessionOptions()
        so.log_severity_level = 3
        return ort.InferenceSession(b, sess_options=so, providers=["CPUExecutionProvider"])

    s_orig, s_new = session(orig_bytes), session(new_bytes)
    worst = 0.0
    for seed in range(5):
        x = np.random.default_rng(seed).random((1, 224, 224, 3), dtype=np.float32)
        ref_in = np.transpose((x - np.float32(0.5)) * np.float32(2.0), (0, 3, 1, 2))
        ref = {r.shape: r for r in s_orig.run(None, {s_orig.get_inputs()[0].name: ref_in})}
        for out in s_new.run(None, {s_new.get_inputs()[0].name: x}):
            worst = max(worst, float(np.abs(ref[out.shape] - out).max()))
    print(f"  surgery bit-exactness across 5 seeds: max abs diff {worst}")
    if worst != 0.0:
        sys.exit(f"NOT bit-exact ({worst}); refusing to write")


def main() -> None:
    POSE_DIR.mkdir(parents=True, exist_ok=True)

    det_orig = download(DETECTOR_URL)
    m = onnx.load_from_string(det_orig)
    inputs, outputs = io_shapes(m)
    assert inputs == [[1, 3, 224, 224]], f"detector upstream input changed: {inputs}"
    assert outputs == [[1, 2254, 1], [1, 2254, 12]], f"detector outputs changed: {outputs}"
    det_new = surgery_nhwc01_input(det_orig)
    verify_bit_exact(det_orig, det_new)
    DETECTOR_PATH.write_bytes(det_new)
    print(f"wrote {DETECTOR_PATH}\n  vendored sha256 {sha256(det_new)}")

    lm = download(LANDMARK_URL)
    m = onnx.load_from_string(lm)
    inputs, outputs = io_shapes(m)
    assert inputs == [[1, 256, 256, 3]], f"landmark upstream input changed: {inputs}"
    assert [1, 195] in outputs and [1, 1] in outputs, f"landmark outputs changed: {outputs}"
    assert [1, 256, 256, 1] in outputs and [1, 117] in outputs, f"landmark outputs changed: {outputs}"
    LANDMARK_PATH.write_bytes(lm)
    print(f"wrote {LANDMARK_PATH} (as-is)\n  vendored sha256 {sha256(lm)}")


if __name__ == "__main__":
    main()
