"""Dev-only spike helper: inspect the MediaPipe hand ONNX models and run
onnxruntime on a deterministic seeded random input, dumping I/O to .npy so the
Rust `tract` spike can diff against it.

Local-only. No network at run time (models are vendored). No API spend.

Run:
    uv run --with onnxruntime --with numpy tools/handtrack-oracle/spike_io.py
"""

import json
import sys
from pathlib import Path

import numpy as np
import onnxruntime as ort

ROOT = Path(__file__).resolve().parents[2]
MODELS = ROOT / "assets" / "models" / "hand"
OUT = ROOT / "tests" / "fixtures" / "hand"
OUT.mkdir(parents=True, exist_ok=True)


def describe(sess: ort.InferenceSession) -> dict:
    return {
        "inputs": [{"name": i.name, "shape": i.shape, "type": i.type} for i in sess.get_inputs()],
        "outputs": [{"name": o.name, "shape": o.shape, "type": o.type} for o in sess.get_outputs()],
    }


def concrete_shape(shape):
    # Replace dynamic/None/str dims with 1 (batch) or a sane default.
    out = []
    for d in shape:
        out.append(d if isinstance(d, int) and d > 0 else 1)
    return out


def run_model(tag: str, path: Path, seed: int) -> dict:
    sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    desc = describe(sess)
    inp = sess.get_inputs()[0]
    shape = concrete_shape(inp.shape)
    rng = np.random.default_rng(seed)
    x = rng.standard_normal(size=shape).astype(np.float32)
    np.save(OUT / f"{tag}_input.npy", x)
    outs = sess.run(None, {inp.name: x})
    out_meta = []
    for idx, (o, arr) in enumerate(zip(sess.get_outputs(), outs)):
        fn = f"{tag}_output_{idx}.npy"
        np.save(OUT / fn, np.asarray(arr).astype(np.float32))
        out_meta.append({"index": idx, "name": o.name, "shape": list(np.asarray(arr).shape), "file": fn})
    return {"input_shape": shape, "input_name": inp.name, "describe": desc, "outputs": out_meta}


def main() -> int:
    manifest = {
        "palm": run_model("palm", MODELS / "palm_detection.onnx", seed=1234),
        "landmark": run_model("landmark", MODELS / "hand_landmark.onnx", seed=5678),
        "ort_version": ort.__version__,
    }
    (OUT / "spike_manifest.json").write_text(json.dumps(manifest, indent=2))
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
