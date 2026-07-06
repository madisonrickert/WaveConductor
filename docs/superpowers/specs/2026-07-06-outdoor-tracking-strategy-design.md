---
title: Outdoor tracking strategy — kiosk hand robustness + full-body dance sketch
date: 2026-07-06
status: decided (kiosk workstream), scoped (body workstream)
context: Priceless festival deployment, ~2 weeks out
---

# Outdoor tracking strategy

Decision record and scoped plan produced after real-world outdoor testing of the
MediaPipe hand pipeline (dappled sun, mixed skin tones, jewelry) came back
disappointing, and while weighing a pivot back to Leap Motion and/or adding
full-body tracking for a new sketch.

## 1. Goals (two problems, one coat)

The work splits cleanly into two tracking problems with different demands:

| | Kiosk installation | Dance-performance sketch (new) |
|---|---|---|
| Subject | Public walk-up, anyone's hands | A performer's whole body |
| Count | 1–2 hands | Likely one performer (staged) |
| Environment | Outdoors, mixed sun, uncontrolled | Staged, rehearsable, controllable light/framing |
| Data needed | Hands (palm + grab/pinch) | Body skeleton (2D joints) |
| Priority | **Robustness is paramount** | **Full-body is the point** |
| Status | Exists; failed outdoors | Does not exist yet |

Shared constraints: ~2 weeks to Priceless; Apple M1 (deploy machine TBD); willing
to buy the right sensor; multi-hour thermal-stable soak; Rust/Bevy with an existing
ONNX Runtime + CoreML inference path.

## 2. Problem analysis

### Why the kiosk hand tracking failed (hypotheses scored)

1. **Sunlight — dominant cause.** The mechanism is mainly the consumer webcam's
   **auto-exposure/white-balance hunting** under high-dynamic-range scenes (bright
   sky / dappled shade behind the hand), plus overexposure crushing low-contrast
   hand texture. MediaPipe's two-stage tracker crops each frame's search region
   from the previous frame's result, so anything that makes the hand's pixels
   *non-stationary frame to frame* (AE/AWB re-adjusting, moving dappled light)
   breaks it. Google's MediaPipe Hands model card independently names low light,
   motion blur, and occlusion as out-of-scope degradation conditions.
2. **Skin tone / low contrast — real but unquantified.** Strong general evidence
   of RGB pose/face models underperforming on darker skin (Sony FHIBE; Google's
   own Monk Skin Tone Scale exists because Fitzpatrick under-covers dark tones),
   plus directional hand-specific evidence — but no published MediaPipe-Hands
   benchmark stratified by skin tone. The field observation is consistent with the
   literature. Front fill light attacks this and the lighting problem at once.
3. **Jewelry — real but secondary.** Named out-of-scope in the model card. A
   contributor, not the headline.

### The reframing insight

The dividing line for outdoor robustness is **passive-visible-light vs.
active-infrared**, not RGB-vs-depth. Sunlight is broadband IR — exactly what
active-IR sensors depend on being able to control.

- **Leap Motion is one of the worst choices for sun**, not a fix. Ultraleap's own
  docs say sunlight degrades tracking and forces a fallback mode. Kinect
  (structured light *and* ToF) and RealSense's L515 LiDAR wash out too.
- Sensors that survive sun are **passive/active stereo** (they fall back to using
  the sun as their illuminant): ZED, RealSense D4xx, OAK-D, Orbbec Gemini.
- The RGB webcam is already in the right family. Its problem is a cheap sensor on
  full auto, not that it's RGB.

Conclusion: pivoting to Leap as the outdoor primary is backwards. Keep Leap as an
indoor/fallback option only.

## 3. Ideas scored

| Idea | Verdict | Why |
|---|---|---|
| Higher MediaPipe inference resolution | Won't help | Palm/landmark models are fixed at 192/224 px (`crates/wc-core/src/input/providers/mediapipe/pipeline.rs:67`). Frame is downsampled regardless of camera resolution. |
| Open-source GPU/ML driver on Leap stereo IR | Not in 2 weeks (multi-month/research) | IR domain gap: open hand models are RGB-trained; MediaPipe on IR is documented as near-unusable (Google issue #2920); no public Leap-IR training set exists. |
| Different modern stereoscopic IR camera | Right direction, wrong axis | The win is *stereo* (degrades to passive in sun), not "IR." OAK-D / Orbbec Gemini / RealSense D4xx qualify; macOS-ARM + 2-week integration risk apply. |
| Old Kinect | Dead end (two grounds) | Active IR fails in sun *and* no skeleton path on Apple Silicon (Kinect body SDKs are Windows/CUDA; libfreenect2 ARM = depth only). |
| Port MediaPipe full-body | Yes — right call for the dance sketch | Do it as 2D RGB pose. BlazePose is single-person; RTMO/MoveNet MultiPose if multiple dancers. |

### Leap corporate reality (validated)

Ultraleap breached its loans, cut ~half its staff in early 2025, and was **acquired
by ROLI (music instruments) in November 2025**, now focused on music tooling rather
than desktop hand-tracking support. The existing project posture (treat as
abandonware; vendored-binary hedge) is correct. On Apple Silicon the thermal cost is
~44% CPU (tolerable; mitigable with Low Power Mode + `LeapSetPause`), not the Intel
90 °C reports — but `leaprs` is a Linux/Windows-targeted binding pointed at a Mac
dylib it was never tested against, an independent fragility.

## 4. Decisions locked (2026-07-06)

1. **Do not pivot to Leap as the outdoor primary.** Keep it as an indoor/fallback
   provider only.
2. **Kiosk robustness = camera + light, not compute or a new sensor class.** Fix
   exposure/white-balance, shade + fill the interaction zone, ROI-crop, and upgrade
   the camera within the same (UVC/RGB) family.
3. **Buy a manual-exposure, global-shutter machine-vision UVC camera** for the
   kiosk (drop-in to the existing AVFoundation path).
4. **Body sketch = 2D RGB pose, monocular, no depth sensor.** Start with
   **BlazePose (single-performer)**; keep the data model general enough to swap in a
   multi-person model (MoveNet MultiPose ≤6, or RTMO for a crowd) later.
5. **Sequence: kiosk robustness first; body sketch as a stretch goal.**

## 5. Workstream A — Kiosk hand robustness (priority)

Ranked by ROI. Directly supported by the research.

### A1. Lock exposure + white balance (highest leverage; real code gap)
The macOS capture path (`crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`)
runs a fixed `AVCaptureSessionPreset640x480` with **auto-exposure only — no manual
controls**. Add manual exposure / gain / white-balance lock (settable, persisted),
which directly kills the dominant outdoor failure mode (AE/AWB hunting).

### A2. Physical: shade/hood + even frontal fill (no code)
Build a shade hood over the interaction zone and add diffuse frontal fill light.
Collapses scene dynamic range so a locked exposure holds, and raises
hand-vs-background contrast (helps the skin-tone axis too).

### A3. ROI-crop the interaction zone before the detector
Crop a fixed interaction region so hands fill more of the fixed 192 px input. This
is the real "resolution" win (raising camera/model resolution is not).

### A4. Buy a manual-exposure, global-shutter machine-vision UVC camera
Drop-in to the existing UVC → AVFoundation path (lowest integration risk). Look for:
global shutter (kills rolling-shutter skew), short exposure capability, WDR/HDR
sensor (≥60 dB), manual exposure/gain/WB over UVC, and a lens/FOV suited to the
interaction volume. A dedicated model shortlist is a follow-up research task.

### A5. Model tuning (config only)
Keep `model_complexity = Full` (not Heavy). Nudge `min_tracking_confidence` up so
the pipeline re-detects before drift compounds.

### Codebase seam notes (A)
- Provider trait `HandTrackingProvider` (`crates/wc-core/src/input/provider.rs:73`)
  is a clean strategy pattern; capture-side changes are localized to the AVFoundation
  source and settings.
- Live-tunable settings already flow lock-free via `MediaPipeLiveTuning`
  (`.../mediapipe/pipeline.rs`); exposure controls should follow that pattern so they
  are adjustable from the dev panel without a restart.

## 6. Workstream B — Dance-performance body sketch (stretch)

Stay RGB, monocular, **2D**, single-shot. Do **not** add a depth sensor: every
depth-skeleton SDK is Windows/Linux + CUDA (none runs on M1) *and* depth fails
outdoors. Full-body pose is far more robust outdoors than hands (large, high-context
targets), and 2D image-space joints are the standard, correct input for generative
visuals (monocular 3D's depth axis is the unreliable one).

### Model
- **Start: BlazePose (single-performer).** Fastest to stand up; mirrors the existing
  hand pipeline topology (two-stage detector → landmark, anchors/NMS decode), with a
  pre-converted PINTO model. Lowest risk.
- **Later, if multiple dancers:** MoveNet MultiPose (≤6, single-pass, Apache-2.0) or
  RTMO (crowd, Apache-2.0 single-pass). Avoid YOLO-pose AGPL for a venue deployment;
  avoid ViTPose/Sapiens (CoreML transformer fallback / non-commercial license).

### Codebase seam notes (B)
- Adding a new provider is easy; **sketches are not coupled to landmark shape** (they
  read palm position + grab/pinch scalars + bone centers only).
- The invasive part: the `Hand` type **hard-codes 21 landmarks**
  (`crates/wc-core/src/input/hand.rs:12`), duplicated in `FusedHand`, the `Landmarks`
  component, and `BoneCenters([Vec3;20])`; `MAX_HANDS = 2`; bone topology is
  hard-coded. A 33-joint body cannot reuse `Hand`. Add a **parallel `Body`/`Pose`**
  data type, frame message, and ECS sync system alongside `Hand`, plus a new sketch
  (register in `AppState`, `SKETCH_ORDER`, `SketchManifest`). It is a contained
  data-model generalization, not a plumbing rewrite.
- Inference reuses the existing `ort` + CoreML path. Profile the CoreML partition
  count early — the CoreML EP is only worth it if it clears the CPU EP by a real
  margin; pure-CNN pose models (BlazePose, MoveNet, RTMPose) partition cleanly.

## 7. Suggested 2-week sequencing

- Days 1–3, kiosk software: A1 exposure/WB lock + A3 ROI crop; expose as settings.
- Days 1–3, kiosk physical (parallel): A2 shade/fill; order the A4 camera.
- Days 4–10, body sketch: parallel `Pose`/`Body` data model + BlazePose RGB provider
  on the existing ort/CoreML path + new sketch; benchmark CoreML partition early.
- Days 10–14: integrate, outdoor soak-test both, tune, confirm thermal stability.

Deployment machine is TBD: a machine with more thermal headroom, or a later OAK-D
(computes pose on-camera), directly helps the multi-hour soak. Treat OAK-D as a
post-festival investment, not a 2-week bet (DepthAI on M1 is fiddly; needs a sidecar
process).

## 8. Open items / risks

- Camera model shortlist (A4) not yet chosen — follow-up research task.
- Deployment machine + thermal headroom undecided.
- Body performer count "not sure yet" — BlazePose-first keeps the model swappable.
- CoreML partition profiling for the pose model is an unknown until benchmarked on
  the actual M1.
- Skin-tone underperformance is real but unquantified; front fill light is the one
  intervention that helps both lighting and low-contrast/skin-tone failures at once.

## 9. Key sources

- MediaPipe Hands model card (out-of-scope: low light, motion blur, occlusion,
  jewelry): storage.googleapis.com/mediapipe-assets/ (Hand Tracking model card, 2021)
- Ultraleap sunlight limitation: support.ultraleap.com (outdoor/bright-environment)
- Ultraleap → ROLI acquisition (Nov 2025): roli.com/blog/ultraleap-is-joining-roli
- MediaPipe on IR near-unusable: github.com/google/mediapipe/issues/2920
- RealSense bright-light performance: dev.intelrealsense.com (tuning depth cameras)
- Depth fails outdoors (Azure Kinect study): pmc.ncbi.nlm.nih.gov/articles/PMC7827245
- Hand-vs-body outdoor dropout (~1000×): arxiv.org/pdf/2306.17558
- BlazePose (single-person, on-device): research.google/blog (BlazePose)
- RTMPose/RTMO (Apache-2.0): github.com/open-mmlab/mmpose
- MoveNet MultiPose (≤6, Apache-2.0): tensorflow.org/hub/tutorials/movenet
- Sony FHIBE skin-tone disparity: nature.com/articles/s41586-025-09716-2
