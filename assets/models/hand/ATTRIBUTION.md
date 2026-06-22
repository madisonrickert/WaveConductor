# MediaPipe hand-tracking model attribution

Both models are derived from Google MediaPipe's hand-tracking solution and are
redistributed under the Apache License 2.0 (see `LICENSE`). They are vendored
here so builds and the unattended deployment never need the network.

## `hand_landmark.onnx`
- **Upstream:** OpenCV Zoo — `opencv/handpose_estimation_mediapipe`
  (`handpose_estimation_mediapipe_2023feb.onnx`).
  https://huggingface.co/opencv/handpose_estimation_mediapipe
- **License:** Apache-2.0.
- **Vendored as-is** (no modification).
- **SHA-256:** `db0898ae717b76b075d9bf563af315b29562e11f8df5027a1ef07b02bef6d81c`
- **I/O:** input `input_1` `[1,224,224,3]`; outputs `[1,63]` image landmarks
  (21×3), `[1,1]` presence, `[1,1]` handedness, `[1,63]` world landmarks (21×3).

## `palm_detection.onnx`  (graph-surgeried derivative)
- **Upstream original:** OpenCV Zoo — `opencv/palm_detection_mediapipe`
  (`palm_detection_mediapipe_2023feb.onnx`).
  https://huggingface.co/opencv/palm_detection_mediapipe
- **License:** Apache-2.0.
- **Original SHA-256:** `78ff51c38496b7fc8b8ebdb6cc8c1abb02fa6c38427c6848254cdaba57fcce7c`
- **Modification:** 26 `PReLU` slope initializers reshaped `[1,C,1,1]` → `[C,1,1]`
  so ONNX Runtime's CoreML EP accepts them, cutting the palm graph from 30 to 6
  CoreML partitions. **Bit-exact** under onnxruntime (max-abs-err 0.0 vs upstream).
  Reproducible via `tools/handtrack-oracle/graph_surgery.py`. (Earlier revisions
  also rewrote the FPN `Resize` nodes `sizes`→`scales` and stripped the
  initializers that orphaned, both for the `tract` runtime; reverted once the
  runtime moved to ONNX Runtime. Full history:
  `docs/runbooks/onnx-coreml-model-surgery.md`.)
- **Vendored (surgeried) SHA-256:** `279efac2c325424f8f262955ffbb4a5702249cef7f4163123f52f2caaee74cf4`
- **I/O:** input `input_1` `[1,192,192,3]`; outputs `[1,2016,18]` raw box/keypoint
  regressions, `[1,2016,1]` raw scores. Anchor decode + NMS are performed in Rust
  (the graph emits raw tensors).

## Upstream lineage
Both are conversions of Google's MediaPipe Hands `.tflite` models
(https://github.com/google-ai-edge/mediapipe), Apache-2.0.
