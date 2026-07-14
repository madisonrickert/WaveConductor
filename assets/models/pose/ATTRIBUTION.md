# BlazePose body-tracking model attribution

Both models are derived from Google MediaPipe's pose solution (BlazePose) and
are redistributed under the Apache License 2.0 (see `LICENSE`). They are
vendored here so builds and the unattended deployment never need the network.
Both were fetched and prepared 2026-07-13 by `tools/pose-models/fetch_and_prepare.py`,
which reproduces these exact bytes (byte-identical re-run verified).

## `pose_detection.onnx`  (graph-surgeried derivative)
- **Upstream original:** OpenCV Zoo — `opencv/person_detection_mediapipe`
  (`person_detection_mediapipe_2023mar.onnx`).
  https://huggingface.co/opencv/person_detection_mediapipe
- **License:** Apache-2.0.
- **Original SHA-256:** `47fd5599d6fa17608f03e0eb0ae230baa6e597d7e8a2c8199fe00abea55a701f`
- **Modification:** the upstream export takes NCHW `[1,3,224,224]` in `[-1,1]`
  (its demo preprocesses `(x/255 - 0.5) * 2` and transposes HWC→CHW), unlike
  every other vendored model here (NHWC, `[0,1]`). The zoo demo's preprocessing
  is prepended inside the graph (`Sub(0.5)` → `Mul(2)` → `Transpose(0,3,1,2)`)
  and the input re-declared as `input_nhwc01` `[1,224,224,3]`, so the vendored
  file presents the family-standard NHWC `[0,1]` contract that
  `fill_nhwc_unit` in the Rust body pipeline produces. **Bit-exact** under
  onnxruntime's CPU EP: `new(x)` vs `orig(transpose((x-0.5)*2))`, max-abs-err
  0.0 across 5 seeded random inputs. Reproducible via
  `tools/pose-models/fetch_and_prepare.py`.
- **Vendored (surgeried) SHA-256:** `3b397f95c15256bf514737507f3ab0e8138baa32824c4f2584c3cc2c5295429a`
- **I/O:** input `input_nhwc01` `[1,224,224,3]` RGB in `[0,1]`; outputs
  `[1,2254,12]` raw box/keypoint regressions, `[1,2254,1]` raw scores. Anchor
  decode (2254-anchor SSD grid), score sigmoid, and NMS are performed in Rust
  (the graph emits raw tensors).
- **CoreML partition check** (per `docs/runbooks/onnx-coreml-model-surgery.md`,
  ORT NeuralNetwork format, 2026-07-13): **4 partitions**, 146/153 nodes on
  CoreML. No `PReLU` rejections (no slope surgery needed). Remaining rejections
  are the known not-bit-exact-fixable classes: `Resize` `half_pixel`, constant
  `Pad` without `constant_value`, 3-D `Concat`.

## `pose_landmark_full.onnx`
- **Upstream:** OpenCV Zoo — `opencv/pose_estimation_mediapipe`
  (`pose_estimation_mediapipe_2023mar.onnx`).
  https://huggingface.co/opencv/pose_estimation_mediapipe
- **License:** Apache-2.0.
- **Vendored as-is** (no modification).
- **SHA-256:** `9d89c599319a18fb7d2e28451a883476164543182bafca5f09eb2cf767ed2f3f`
- **I/O:** input `input_1` `[1,256,256,3]` RGB in `[0,1]`; outputs `[1,195]`
  image landmarks (39 rows × 5: 33 body + 6 auxiliary), `[1,1]` presence,
  `[1,256,256,1]` segmentation mask, `[1,64,64,39]` heatmap (unused),
  `[1,117]` world landmarks (39×3).
- **CoreML partition check** (same procedure, 2026-07-13): **6 partitions**,
  190/196 nodes on CoreML — the same 6-partition floor as the vendored hand
  models, with the same rejection classes (`Resize` `half_pixel`, constant
  `Pad`). No `PReLU` rejections.
- **Variant note:** the upstream README does not state which BlazePose variant
  (lite/full/heavy) this 5.6 MB export derives from; its size suggests
  lite-or-full-fp16 heritage. Adopted anyway: it is the same trusted conversion
  lineage as the vendored hand models and emits the 33-landmark + 256² mask
  outputs the pipeline requires. The filename names the *slot* — if hardware
  testing shows insufficient tracking quality, re-vendor the explicit `full`
  variant from the PINTO model zoo (`053_BlazePose`) into the same filename
  (re-checking its input layout/range first) and re-run the model contract
  tests; no code change.

## Upstream lineage
Both are conversions of Google's MediaPipe BlazePose `.tflite` models
(https://github.com/google-ai-edge/mediapipe), Apache-2.0.
