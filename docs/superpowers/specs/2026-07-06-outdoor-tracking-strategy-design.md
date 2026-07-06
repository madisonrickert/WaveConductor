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
4. **Body sketch = 2D RGB pose, monocular, no depth sensor.** Default to a
   **multi-person model (MoveNet MultiPose, ≤6)** because the body sketch is now
   expected to run in kiosk mode where several walk-ups engage at once — a hard
   multi-person requirement BlazePose cannot meet. Build a **general `Pose` data
   model + swappable pose provider** so a high-fidelity single-person **BlazePose**
   provider can be added for a solo staged performance, and **RTMO** for genuine
   crowds >6, without disturbing sketches.
5. **Sequence: kiosk robustness first; body sketch as a stretch goal.**

## 5. Workstream A — Kiosk hand robustness (priority)

Ranked by ROI. Directly supported by the research.

### A1. Lock exposure + white balance (highest leverage; real code gap)
The macOS capture path (`crates/wc-core/src/input/providers/mediapipe/capture/avfoundation.rs`)
runs a fixed `AVCaptureSessionPreset640x480` with **auto-exposure only**.

**Critical platform finding — the lock does NOT go through AVFoundation.** On macOS,
AVFoundation exposes almost no UVC control for *external* webcams:
`isExposureModeSupported(.custom/.locked)` returns false for external UVC devices, and
`setExposureModeCustomWithDuration:ISO:` is effectively a built-in/iOS-camera feature.
So you cannot pin exposure/gain/WB on an external kiosk camera through the
AVFoundation session — it only gives you the camera firmware's auto mode, which is
exactly the hunting seen in the field. My original "add manual exposure to the
AVFoundation config" framing was wrong for the deployment camera.

**Correct mechanism:** drive the camera's UVC **Camera-Terminal / Processing-Unit**
controls out-of-band from AVFoundation — set `CT_AE_MODE` to manual, then
`CT_EXPOSURE_TIME_ABSOLUTE`, `PU_GAIN`, and `PU_WHITE_BALANCE_TEMPERATURE` (auto off).
Apply *after* the AVFoundation session starts, and **re-apply on every (re)start**,
since acquiring the device can reset controls.

Implementation choice (for the plan):
- **Preferred: native IOKit control requests via the existing `objc2` macOS layer.**
  The capture module already uses objc2/AVFoundation; IOKit is the macOS-correct way
  to send UVC control transfers without claiming the streaming interface AVFoundation
  holds. Adds **no new crate and no external binary** (respects the avoid-new-deps and
  agent-first preferences). More upfront work: walk IOKit to the matching USB device
  and issue `SET_CUR` transfers with the right unit IDs / selectors.
- **Faster-to-prototype fallback:** bundle and shell to `uvc-util` (IOKit-based).
  Proven and quick, but a loose external binary — against the harness ethos.
- Reference implementations to port from: `uvc-util`, `VVUVCKit`; the `uvcc` CLI
  documents the exact Logitech control set (the Brio is its reference device — see A4,
  which is why the Brio de-risks this task).

**Must-validate on hardware** (device- and macOS-version-specific): confirm the
control write *holds* while AVFoundation streams, across several minutes of changing
sun. Test this on the actual M1 + chosen camera before committing to a deployment.

Note on the multi-hour moving sun: UVC has no standard ROI-metered auto-exposure
control, so the answer is a manual value with dynamic-range headroom **plus an
HDR-capable camera** (A4) whose sensor holds highlights, and optional periodic
re-tuning — not an AVFoundation metered-auto mode (unavailable for external cams).
The built-in M1 webcam *may* accept AVFoundation custom exposure (built-in-camera
feature), which is useful only as a stopgap test; the deployment path is external
camera + the out-of-band control above.

**Shortcut that may make this whole task unnecessary for the festival:** the
already-owned **OBSBOT Tiny 2 Lite** persists a manual exposure/gain/WB lock in its own
firmware (set once in OBSBOT Center, holds with the app closed — see A4). If it
soak-tests clean in mixed sun, the app never needs to set exposure at all, and the
IOKit UVC control work here is **deferred/unneeded** until you switch to a plain webcam
(Brio/C920) or want runtime control. Build the IOKit path only on the fallback camera
route.

### A1b. Camera preview + live tuning panel (operator's instrument)
**Yes — we need this, and it is not gold-plating.** A locked/metered exposure is only
as good as the value/region the operator picks, and the right choice depends on the
actual on-site light, which can only be judged live. Ultraleap gave you a preview in
its control panel; the webcam path has no equivalent. Build a minimal egui panel,
gated behind the existing dev/ADVANCED toggle, that shows the raw camera frame plus
live sliders for exposure/gain/WB, the metering-ROI / interaction-crop rectangle
(see A3), and a detection indicator (is a hand being found, and its confidence). The
frame already flows through the pipeline; surfacing it as an egui texture is modest.
This panel is the tuning instrument that makes A1 and A3 usable in the field, and it
doubles as an on-site diagnostic. It fits the existing egui-settings choice and the
dev-settings ADVANCED pattern.

### A2. Physical: shade the *zone*, and fill it (partial control accepted)
Fully controlling the ambient light at a public outdoor installation is impractical
(sun moves, people approach from any angle, weather varies). So reframe: you do not
need to shade the *area*, only the ~arm's-length **interaction zone the camera
actually images**. The kiosk enclosure / an awning / parasol shading that small
volume is achievable and helps. Frontal **fill light** is the more controllable half
of this and is where to invest: a diffuse panel on the kiosk aimed at the hand zone
raises hand-vs-background contrast (helps the skin-tone axis) and is easy to deploy.

Consequence of accepting weak shade control: the weight shifts *onto* A1 (exposure)
and A4 (camera). If the sensor must handle the full outdoor dynamic range unaided,
the **camera hardware upgrade becomes the primary lever, not a nice-to-have**, and
ROI-metered exposure (A1 mode 1) matters more than a single fixed lock. Do not treat
A2 as a prerequisite for the software fixes; treat it as additive.

### A3. Feed the detector more pixels-per-hand — without sacrificing FOV
Constraint (operator): the built-in Mac webcam FOV is already tight; do **not** shrink
the interaction volume. So this is reframed away from "crop tighter."

Root cause is two things stacking: the palm detector ingests a fixed ~192 px square,
**and** the capture path is pinned to `AVCaptureSessionPreset640x480`. A 480 px frame
downsampled to 192 px starves a hand at arm's length of pixels regardless of framing.
Two FOV-neutral levers fix it:

1. **Raise capture resolution (the real lever, costs no FOV).** The Brio does 1080p/4K
   vs today's pinned 480p. A crop of a 4K frame carries far more hand detail than a
   crop of a 480p frame *at the same physical FOV*. Un-pinning the 640×480 preset is a
   bounded code change and is the prerequisite for any crop to help.
2. **Wider FOV from the external camera, not a crop.** The Brio's selectable 90° is
   much wider than the stock Mac cam, so switching cameras *gains* interaction volume.

The crop then becomes **adaptive and optional**, not a fixed FOV sacrifice: capture
wide + high-res, and feed the detector a crop of wherever hands actually are, kept as
large as the deployment allows. You choose how much of a high-res wide frame the
fixed-size detector sees; the user's interaction volume stays equal to the (wide)
capture FOV. Only crop tighter if, after the resolution/FOV upgrade, hands are still
too small.

Consistency with the earlier research nuance: raising the *number* alone buys nothing
if you still downsample the whole frame to 192 — resolution pays off **through** a
crop. Higher capture res + a well-placed crop is the combination.

Notes:
- The crop pairs with the A1 *manual* exposure choice (not auto-metering, which UVC
  external cams don't expose): the crop region's brightness informs the exposure value
  you set via A1b. Crop and exposure reinforce each other.
- Placement uses the A1b preview.
- Tradeoff to watch: capturing/resizing 4K costs more bandwidth than 480p; keep the
  resize scratch buffers pre-allocated (the pipeline already reuses `square_pad_into` /
  `resize_into`), per the no-hot-path-allocation rule.
- Codebase: `ContentRect` / square-pad + per-ROI warp already exist, so inserting a
  configurable crop rect ahead of them is bounded.
- If Workstream B adds body pose later, "body pre-focusing" (crop around detected
  wrists) becomes available for free — but for a fixed-zone kiosk a static
  operator-set crop is simpler; do not add a body model just for this.

### A4. Camera pick: test the already-owned OBSBOT first, Brio as fallback
General verdict first (corrects the earlier "global shutter" instinct): the failure is
auto-exposure hunting + low dynamic range, **not** motion skew; at arm's length,
short exposure, rolling-shutter skew is negligible. Global shutter fixes a problem you
don't have while costing HDR — and the cheapest GS modules (OV2311/OV9281) are
**monochrome**, which starves MediaPipe's RGB models. So the target is a **color,
rolling-shutter, HDR** camera whose manual exposure can be **locked and held**.

**Owned option (test first, zero cost): OBSBOT Tiny 2 Lite.** 4K, HDR, UVC, 79.4° FOV,
rolling shutter. It is architecturally an AI talking-head cam (mechanical gimbal +
on-camera AI + gesture logic), which is *wrong* for a fixed kiosk — but it can be
provisioned into a locked static state, and **those settings persist in firmware
without the OBSBOT app running** (OBSBOT FAQ confirms settings survive restart /
reconnect / machine change). One-time provisioning in OBSBOT Center:
1. Manual exposure (shutter), manual ISO/gain, manual white balance.
2. **HDR off** and any auto image modes off (they layer over exposure).
3. Lock focus (PDAF would otherwise hunt).
4. **Initial Working State** = fixed gimbal position + fixed zoom + no-tracking mode
   (boots to this locked frame, no app needed).
5. **Gesture control OFF** (else users' hand gestures self-trigger its zoom/tracking).
Then AVFoundation/MediaPipe streams from it with the app closed. **Must soak-test in
real mixed sun that the locked exposure holds and the AI does not re-assert.** Risks:
narrower 79.4° FOV; gimbal is a moving part with no weather sealing (outdoor-reliability
question); direct standard-UVC exposure locking from our own app is *unverified* for
this model (rely on OBSBOT Center provisioning + firmware persistence, not a scripted
`uvc-util` lock). **This is the biggest timeline lever: if the OBSBOT holds, A1's IOKit
UVC control code is unnecessary for the festival** (see A1, Section 7).

**OBSBOT SDK** (access requested): not needed for the kiosk (firmware-persist path
covers it). Earns its place only for runtime control, or for the body *performance*
where the auto-tracking gimbal becomes a feature (camera follows the dancer — see
Workstream B). Weigh the closed-vendor-SDK dependency with the same caution as Leap
(abandonment / build-cost risk) before taking it on.

**Fallback to buy if the OBSBOT test fails: Logitech Brio 4K (~$50-90 used/refurb).**
True HDR (RightLight 3), selectable 65/78/90° FOV, and — decisively — the **documented
reference device for `uvcc`** (VID 0x46d / PID 0x82d), so A1's out-of-band IOKit UVC
control is a copy-pasteable known-good path. Cheaper stopgap: **C920/C920S (~$20-30
used)**. Permanent-mount step-up: **e-con See3CAM_24CUG (~$180-200 new)** (color GS +
ISP + enclosure). **Skip:** any mono module; any GigE/USB3-Vision/vendor-SDK
machine-vision cam (not plug-and-play UVC on macOS).

### A5. Model tuning (config only; low-priority fine-tuning)
Two knobs, both secondary to A1–A4. Explained so the tradeoffs are clear:

- **Landmark model tier (Lite vs Full).** In your stack this is *which ONNX file you
  ship* (`hand_landmark.onnx`), not a runtime flag. **Full** = more accurate/robust
  landmarks; **Lite** = cheaper/cooler but noisier. Pro of Full: precision and
  robustness, which is what a robustness-paramount kiosk wants. Con: more
  compute/heat, which matters for the multi-hour thermal target — but your
  `frame_limiter` work already de-saturates the GPU, so Full's cost is affordable.
  Verdict: ship Full. (MediaPipe *Hands* is Lite/Full only; "Heavy" is a Pose tier,
  not applicable here.)
- **Re-detection / tracking-confidence threshold.** This maps to the presence
  threshold your custom Rust pipeline uses to decide when to abandon the tracked ROI
  and re-run full palm detection. **Higher** → re-detects sooner when a frame goes
  bad, so it *recovers from drift faster* in harsh conditions — but re-detection runs
  the palm detector more often (more compute/heat) and can flicker if detection is
  momentarily uncertain. **Lower** → sticks with the tracked ROI longer (smoother,
  cheaper) but can *drift* on a stale/wrong position before recovering. Outdoors,
  where frames are frequently bad, leaning slightly higher favors recovery over
  drift. This is a field-tuning dial, not a headline fix — set it empirically once
  A1–A4 are in place, and only if you observe drift-vs-recovery problems.

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

### Model (multi-person is now the likely real requirement)
The body sketch is expected to run in kiosk mode, where multiple people will try it
at once. That flips the default: a single-person model is disqualified for the kiosk
case, whereas a multi-person model also handles the solo-performance case fine (it
just tracks one person). So:

- **Default: MoveNet MultiPose Lightning.** Up to 6 people, single-pass constant-time,
  Apache-2.0, no separate detector. 17 COCO keypoints — plenty for skeleton-driven
  particle emitters / joint forces / silhouette effects (finger detail is the hand
  pipeline's job, not body pose). CoreML conversion via the PINTO zoo; pin a fixed
  input resolution.
- **Optional high-fidelity single-person provider: BlazePose.** For a solo staged
  performance you may want its extra landmarks and smoothness. Because we build a
  general `Pose` type + a swappable pose provider (mirroring the existing
  Auto/Leap/MediaPipe/Off hand-provider dropdown), BlazePose can be a second provider
  selected per deployment context — architecturally cheap, but a stretch-within-a-
  stretch. Not required for the festival.
- **Crowds >6: RTMO** (Apache-2.0 single-pass) if walk-up density exceeds MoveNet's
  6-person cap. Avoid YOLO-pose AGPL for a venue deployment; avoid ViTPose/Sapiens
  (CoreML transformer fallback / non-commercial license).

**Verdict on your BlazePose-vs-MultiPose question:** for *strictly one* dancer,
BlazePose is higher-fidelity (more joints, smoother, ordinal depth). But a
multi-person model covers both the solo and the multi-walk-up cases acceptably, while
single-person BlazePose cannot scale up. Given multiple people will use it, MoveNet
MultiPose is the safer single default; keep BlazePose as an optional add-on provider.

### Modes & external output (free-play, scripted, Resolume)
Two envisioned modes:
- **Free-play** (kiosk-like): interactive, multiple walk-ups → MultiPose. The in-app
  Bevy sketch, driven by body tracking.
- **Scripted** (for a performance): choreographed/cued — via an internal timeline, or
  externally (see Resolume).

**Resolume as an option.** WaveConductor could output to Resolume and let it handle
sequencing/compositing for the performance. macOS/Resolume-standard interop, both
additive:
- **Syphon (video):** WaveConductor exposes a Syphon server; Resolume ingests the
  rendered sketch as a layer. Syphon is objc-based — reachable via the existing objc2
  layer, no heavy new dep.
- **OSC (data/control):** send tracking-driven events as OSC to Resolume (drive its
  params/triggers), and/or receive OSC from Resolume to cue WaveConductor sketch states
  during the show. Small crate (`rosc`) or a thin sender; `ProviderId::WebSocket` is an
  existing external-I/O precedent.

**Scope fork for the performance path (decide when Workstream B starts):**
- (a) *WaveConductor renders*, Resolume composites/cues → in-app body sketch + a
  Syphon/OSC output layer.
- (b) *Resolume renders*, WaveConductor is the tracking engine → robust body tracking +
  OSC/Syphon output, Resolume does visuals + scripting. Potentially a **smaller**
  performance build (no rich new Bevy sketch); the free-play/kiosk case still wants the
  in-app sketch.

Both share the general `Pose` model + a clean output layer. A WaveConductor→Resolume
bridge (Syphon + OSC) is a **general capability** that could serve all sketches, not
just body — build it as its own module if pursued. Resolume integration is almost
certainly **post-festival**, not the 2-week critical path. Near-term design
implication: keep the `Pose` type serialization-friendly so an OSC/Syphon output layer
drops on cleanly later.

**OBSBOT gimbal as a performance feature.** The auto-tracking gimbal that is a
*liability* for the fixed kiosk is a potential *feature* for a solo scripted
performance: the camera physically follows the dancer, keeping them centered/framed.
Reachable via the OBSBOT SDK (access requested). This is a performance-only,
post-festival idea, and it carries a closed-vendor-SDK dependency (weigh as with Leap);
noted so the option isn't lost. It pairs naturally with the single-person BlazePose
provider, not the multi-person kiosk default.

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

- Day 0-1, camera decision (FIRST, zero code): provision the already-owned OBSBOT
  Tiny 2 Lite in OBSBOT Center (manual exposure/ISO/WB, HDR off, focus lock, Initial
  Working State = fixed frame + no-tracking, gesture control OFF) and outdoor
  soak-test that the locked exposure HOLDS while AVFoundation streams with the app
  closed. This gates the code plan:
  - Holds → **skip A1's IOKit UVC control for the festival**; kiosk code = A3 + A1b +
    tuning only. Big timeline win.
  - Fails → fall back to a Brio-class webcam and build A1's IOKit UVC control.
- Days 1–4, kiosk software: A3 (un-pin 640×480 → higher capture resolution + adaptive
  crop) + A1b preview panel + tuning; **plus A1 IOKit UVC control only on the fallback
  (plain-webcam) path**. Expose as settings.
- Days 1–3, kiosk physical (parallel): A2 shade/fill; buy a Brio only if the OBSBOT
  test fails.
- Days 4–10, body sketch: parallel `Pose`/`Body` data model + MoveNet MultiPose RGB
  provider on the existing ort/CoreML path + new sketch; benchmark CoreML partition
  early. (BlazePose single-person provider is an optional later add.)
- Days 10–14: integrate, outdoor soak-test both, tune, confirm thermal stability.

Deployment machine is TBD: a machine with more thermal headroom, or a later OAK-D
(computes pose on-camera), directly helps the multi-hour soak. Treat OAK-D as a
post-festival investment, not a 2-week bet (DepthAI on M1 is fiddly; needs a sidecar
process).

## 8. Open items / risks

- Camera (A4) — **test the already-owned OBSBOT Tiny 2 Lite first** (provision +
  soak-test, zero cost). Fallback to buy if it fails: Logitech Brio 4K (proven UVC
  control); C920 stopgap; See3CAM_24CUG permanent-mount step-up.
- **macOS AVFoundation cannot set manual exposure on external UVC cams** — A1 needs
  out-of-band IOKit UVC control. **But the OBSBOT firmware-persist path may make A1's
  code unnecessary for the festival** — the OBSBOT soak-test result gates whether the
  IOKit work is on the critical path at all. Biggest correction from the original
  design.
- OBSBOT SDK access requested — not needed for the kiosk; relevant only to runtime
  control or the body-performance gimbal-follow (weigh vendor-dependency risk).
- Deployment machine + thermal headroom undecided.
- Body use context: confirmed to include multi-person kiosk walk-ups → MoveNet
  MultiPose default; the general `Pose` model keeps BlazePose swappable for solo work.
- Body modes: free-play + scripted-for-performance; Resolume output (Syphon + OSC) is a
  post-festival option with a (a) in-app-renders vs (b) Resolume-renders scope fork to
  decide when Workstream B starts. Keep `Pose` serialization-friendly meanwhile.
- A3 requires un-pinning the fixed `AVCaptureSessionPreset640x480` capture resolution
  (FOV-neutral pixels-per-hand win); the crop is adaptive/optional, not a FOV sacrifice.
- Camera preview/tuning panel (A1b) added to scope as the operator's on-site
  instrument.
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
