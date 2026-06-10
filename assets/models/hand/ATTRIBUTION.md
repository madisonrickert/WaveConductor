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
- **Modification:** the two FPN `Resize` nodes were rewritten from the ONNX
  `sizes`-input form (which `tract` 0.21 does not honour) to an explicit
  `scales=[1,1,2,2]` 2× upsample. This is **bit-exact** under onnxruntime
  (max-abs-err 0.0 vs the original on identical input). Reproducible via
  `tools/handtrack-oracle/graph_surgery.py`.
- **Vendored (surgeried) SHA-256:** `834842ed98870b72619d7d8284a8cde107fca89dd70041ef3b99799faac7f319`
- **I/O:** input `input_1` `[1,192,192,3]`; outputs `[1,2016,18]` raw box/keypoint
  regressions, `[1,2016,1]` raw scores. Anchor decode + NMS are performed in Rust
  (the graph emits raw tensors).

## Upstream lineage
Both are conversions of Google's MediaPipe Hands `.tflite` models
(https://github.com/google-ai-edge/mediapipe), Apache-2.0.
