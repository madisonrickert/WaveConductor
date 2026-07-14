# Radiance — dancer-aura sketch design

**Date:** 2026-07-11
**Status:** Approved (brainstorm with Madison, 2026-07-11)
**Branch:** `radiance` (worktree `.worktrees/radiance`, based on `v5-alpha` @ 913d3d1e)

## Summary

Radiance is a new sketch: a webcam-tracked dancer's silhouette rendered as a stylized
dark form, with GPU-accelerated psychedelic radiant particles emanating from the
silhouette edge like an aura or flame — driven by the dancer's motion and by live
audio from a selectable input device. Unlike existing sketches, Radiance does not
generate its own audio; it *listens*.

## Decisions locked during brainstorming

| Question | Decision |
| --- | --- |
| Deployment | Both single-performer and party/kiosk; **kiosk is the robustness/thermal bar** |
| Audio source | Both line-in and room mic; **mic is the analysis-quality bar** (AGC required) |
| Silhouette treatment | **Stylized silhouette + aura** (dark glassy fill, emissive rim); no raw camera pixels |
| Dancer count | **One primary dancer**; others in frame ignored (multi-person is follow-up work) |
| Hand tracking coexistence | **Body replaces hands while Radiance is active** (camera arbitration below) |
| Name | **Radiance** (`AppState::Radiance`, storage key `radiance`) |
| Simulation | **Particles + analytic curl-noise flow** (reuse shared particle engine); true fluid sim explicitly rejected for thermal risk |

## Goals

- Robust single-dancer silhouette + body-landmark tracking from a webcam, GPU-accelerated
  (CoreML/ANE on macOS via the existing `ort` stack), tolerant of kiosk conditions
  (variable lighting, people wandering through frame, multi-hour runs).
- Audio-input reactivity: RMS/bands/onsets from a user-selectable capture device,
  robust to room-mic signal quality.
- Aura visual: additive HDR particles born on the silhouette edge, advected by
  curl-noise flow with buoyancy, modulated by audio and limb motion.
- Compute-lite attract/screensaver mode matching the established
  `SketchActivity::Idle → Screensaver` framework.
- Full compliance with established sketch patterns (settings derive, lifecycle
  contract, no-alloc hot paths, ExtractResource removal systems, parity tests).

## Non-goals

- Multi-person tracking (follow-up; design keeps the seam obvious but builds nothing).
- True Navier-Stokes fluid (rejected: new ping-pong texture infra + pressure-solve
  iterations are the highest-thermal-risk option for the multi-hour soak target).
- Raw camera pixels on screen.
- Hand/finger-level gestures inside Radiance (wrist/ankle/hip/head landmark motion is
  the gesture surface; body landmarks include wrists).
- WASM support in v1 (`ort` is native-only; a websocket body provider, mirroring the
  hand-tracking wasm bridge, is the documented later path).
- Audio *output* (Radiance is silent; the master audio engine is untouched).

## Architecture: three units, three implementation plans

Each unit is independently buildable and testable; audio (Plan A) and body tracking
(Plan B) touch disjoint files and can proceed in parallel; the sketch (Plan C)
consumes both.

```
 cpal input stream ──rtrb──▶ AudioAnalysis (Res) ──┐
                                                    ├──▶ Radiance sketch systems
 webcam ▶ worker: detector → BlazePose ──rtrb──▶ BodyTrackingState (Res) ──┤
          (landmarks + 256×256 mask)              MaskTexture (Handle<Image>)──┘
                                                  edge-point list (storage buffer)
```

---

## Unit A: Audio input + analysis (`crates/wc-core/src/audio/input/`)

A second cpal stream mirroring the output engine's architecture in reverse. No new
crates: `cpal` does input, `fundsp` covers FFT (the only FFT-capable crate in the
graph; `rustfft` was deliberately removed 2026-06-20).

- **Capture:** `device.build_input_stream(...)` on the selected device, wrapped as a
  non-send resource (cpal streams are `!Send` on macOS), exactly like `AudioStream`.
  The input callback is real-time-clean: pre-allocated scratch only, pushes samples
  into an `rtrb` ring, errors flip an `Arc<AtomicBool>` (mirrors `AudioErrorFlag`).
- **Analysis:** a `PreUpdate` system drains the ring into a pre-allocated circular
  buffer and computes per frame:
  - RMS + peak;
  - slow AGC (attack/release-asymmetric) normalizing room-mic levels — *mic is the bar*;
  - ~8 log-spaced spectral bands from a windowed FFT (fundsp), post-AGC;
  - onset strength via spectral flux, plus a debounced beat flag.
  Window/hop sized so 60 Hz frame drain always keeps up; all buffers init-allocated.
- **Publication:** `Res<AudioAnalysis> { rms, gain, bands: [f32; 8], onset, beat_confidence }`.
  This is the app's first audio→visual data path (existing coupling is visual→audio only).
- **Device selection:** an `AvailableAudioInputDevices(Vec<String>)` resource populated
  by a cpal enumeration system, registered via `register_runtime_enum_options` under
  `OPTIONS_KEY = "audio_input_devices"` — the exact use case the runtime-enum registry
  module docs anticipate. Bound to a `SettingKind::RuntimeEnum` field on
  `RadianceSettings`; the dropdown widget, TOML persistence, and "Unavailable"
  handling already exist. Empty/default value = system default input device.
- **Lifecycle:** stream is built `OnEnter(AppState::Radiance)` and torn down `OnExit`
  (no open mic outside Radiance). Paused during `Idle`/`Screensaver` (attract mode is
  not audio-reactive). Device-setting change rebuilds the stream via the standard
  `requires_restart` sketch-reload path.
- **Failure posture:** missing/failed device → `AudioAnalysis` holds neutral values
  (zeros, gain 1.0) and status surfaces in diagnostics; the sketch keeps running on
  motion drive alone. Never panic, never block.

## Unit B: Body tracking (`crates/wc-core/src/input/body/`)

A **parallel seam** beside hand tracking, not a variant of `HandTrackingProvider`
(that trait bakes in 21-landmark hands, `MAX_HANDS`, `TrackedHand`). One provider is
planned, so no premature trait: a `BodyTrackingPlugin` owning one worker thread,
copying the proven mediapipe worker shape — `SourceFactory` deferring `!Send` camera
construction to the worker, `rtrb` result ring, newest-frame-wins frame dropping
(never sleeping), idle inference throttle, live-tuning via atomics.

- **Model pipeline:** MediaPipe **BlazePose (Pose Landmarker), `full` variant** —
  two-stage person detector (224×224) → ROI crop → landmark model (256×256) emitting
  **33 landmarks, world landmarks, and a 256×256 person segmentation mask** in one
  pass. Apache-2.0. ONNX sourced from the PINTO model zoo (`053_BlazePose`) or
  opencv's HF mirror; models live in `assets/models/pose/`, loaded via
  `platform::assets::asset_root()`. Same architecture family as the hand models:
  expect the PReLU `[1,C,1,1]` → `[C,1,1]` slope-reshape surgery per
  `docs/runbooks/onnx-coreml-model-surgery.md`, and reuse the per-model CoreML cache
  key. Detect-then-track: detector re-runs only when track is lost.
- **Capture reuse:** promote `providers/mediapipe/capture/` (the `FrameSource` trait,
  AVFoundation and nokhwa backends, `MockFrameSource`) to a shared
  `input/capture/` module consumed by both modalities. Pure move + import updates.
- **Feature flags:** `body-tracking-mediapipe` (ort + image; CI-testable headless) and
  `body-tracking-camera` (capture backends), mirroring the hand-tracking flag split.
- **Ring transport:** landmarks/status cross as POD. The 256 KB mask uses a
  **two-ring buffer pool**: worker→main ring carries filled mask buffers, main→worker
  ring returns empties for reuse — steady state allocates nothing (AGENTS.md hot-path
  rule; the worker loop is a hot path).
- **Main-thread surface:**
  - `Res<BodyTrackingState>`: `present: bool`, screen-normalized landmarks
    `[Vec3; 33]` + per-landmark visibility, world landmarks, smoothed landmark
    velocities, track confidence, provider status/diagnostics (backend label so a
    silent CPU fallback is visible, matching hand-tracking practice).
  - `MaskTexture(Handle<Image>)`: reused `R8Unorm` 256×256 image; mask bytes written
    in place each body frame (Bevy re-uploads on mutation; 256 KB is trivial).
  - **Edge-point list**: the worker extracts up to `MAX_EDGE_POINTS = 2048`
    `(position, outward normal)` pairs where the (EMA-smoothed) mask crosses 0.5 —
    a single 256×256 scan, negligible cost. Uploaded as a storage buffer for the
    particle kernel; doubles as the silhouette rim source.
- **Smoothing:** One-Euro on landmarks (reuse the existing `smoothing.rs` pattern at
  poll rate); temporal EMA on the mask (worker-side) to suppress mask flicker.
- **Presence → attract:** a body-bearing frame resets `InteractionTimer` exactly as
  hand-bearing frames do in `reset_on_interaction`; empty frames are ignored. During
  `Idle`/`Screensaver` the worker drops to detector-only at the idle rate (4 Hz-class,
  with capture throttle) so a person walking up re-activates the sketch. This uses
  the existing sanctioned always-on-listener pattern; the systems gate internally.
- **Camera arbitration:** `OnEnter(Radiance)`: if the active hand provider is
  MediaPipe (webcam), stop it via the existing registry/selection machinery and mark
  it suspended in status; `OnExit`: restore the prior selection. Leap is untouched.
  Radiance itself never reads hand data.
- **Fallback model:** if BlazePose's tracked-crop proves lossy on fast whole-body
  motion in practice, **MoveNet Lightning** (single-shot, dance-trained, Apache-2.0)
  is the documented landmark fallback — but it has no mask, so it supplements rather
  than replaces. Not built in v1.
- **Rejected:** RVM (RobustVideoMatting) — best-in-class matting but GPL-3.0 and
  structurally dynamic ONNX I/O (CoreML EP hostile). SelfieSegmenter/MODNet/
  PP-HumanSeg — portrait-domain, collapse at full-body kiosk distance. YOLO-seg —
  AGPL-3.0 + heavy.

## Unit C: The Radiance sketch (`crates/wc-sketches/src/radiance/`)

Follows the canonical sketch module shape (flame is the closest reference).

### Simulation

Reuses the shared particle foundation — `Particle` POD layout, billboard
`ParticleMaterial` render path, compute-plugin structure, offset-parity test style —
with its **own kernel** (`assets/shaders/radiance/simulate.wgsl`) and its own
`RadianceSimParams` uniform, because the behavior differs from Line/Dots:

- **Emission:** dead particles respawn at hashed indices into the edge-point storage
  buffer, offset along the outward normal, with initial velocity = normal direction
  + audio-scaled speed. Emission rate (respawn probability per frame) is a param.
- **Advection:** analytic curl noise (2–3 octaves, time-scrolled) + constant upward
  buoyancy for the flame-like rise; drag; lifespan fade.
- **Motion drive:** per-frame limb impulses — the CPU baker takes the smoothed
  velocities of wrists, ankles, hips, head and writes up to 8 `(position, velocity)`
  impulse slots into `RadianceSimParams` (same fixed-slot idiom as `Attractor[8]`);
  the kernel adds locally-weighted impulse velocity so a fast limb sheds a burst.
- **Audio drive (CPU-baked into params each frame):** bass bands → emission rate +
  buoyancy pulse; high bands → turbulence amplitude + sparkle; onset → brief radial
  burst gain; slow RMS → master intensity. All parameters, no GPU branching on audio.
- Dispatch size scales with the particle-count setting (no unused workgroups).

### Look

- Additive blending into the single global HDR `Camera2d` (`(One, One)` color target
  override, flame's recipe): overlapping soft discs accumulate into luminous cores;
  existing bloom + tonemapping supply the radiance. **No new post-process pass in v1.**
- **Silhouette fill:** a screen-filling `Material2d` quad under the particles sampling
  `MaskTexture`: smoothstep-edged dark glassy fill (deep translucent gradient +
  audio-shimmered value noise) and a thin emissive rim in the mask's edge band.
- Mirrored horizontally by default (it's a mirror for the dancer); settings toggle.
- Palette: a small set of curated psychedelic gradients (reuse the palette idiom from
  Line), audio-shifted along the gradient.

### Attract / screensaver

`RadianceScreensaverPlugin`, systems gated `in_screensaver(AppState::Radiance)`,
running under the existing thermal present-rate throttle (~20/15/3 fps by tier):

- A **phantom performer**: an analytic SDF silhouette (drifting ellipse cluster —
  torso/head/limbs blobs) generates a synthetic mask + edge list through the same
  CPU path the real tracker uses, so the particle kernel is unchanged.
- Low particle count, ember palette, slow drift. No audio; camera at detector-only
  idle rate purely for presence detection.
- `OnEnter(SketchActivity::Idle)` freeze hook zeroes emission (particles fade out,
  then the throttled last frames hold, matching flame's freeze idiom).

### Settings (`RadianceSettings`, storage key `radiance`)

- **User:** particle count (requires_restart), emission rate, flow strength, buoyancy,
  palette, audio sensitivity, silhouette fill intensity, rim glow, mirror toggle,
  audio input device (`RuntimeEnum { options_key: "audio_input_devices" }`,
  requires_restart so a device change tears down and rebuilds the stream).
- **Dev (Shift+D, behind the ADVANCED toggle):** mask threshold, mask EMA factor,
  mask debug overlay (draw raw mask), edge-point debug, inference backend + fps
  readouts, One-Euro tuning, curl octaves.
- House pattern throughout: `#[setting(...)]` + matching `#[serde(default = ...)]`
  fns + the two defaults-match tests.

### Core wiring checklist

- `AppState::Radiance` variant + `SketchActivity` `#[source]` arm + `SKETCH_ORDER`
  + `next_sketch`/`prev_sketch` + `from_name` (the `Waves` seam stays reserved,
  per the 2026-07 audit decision).
- `register_sketch_settings::<RadianceSettings>()`, `register_sketch_tile(...)`
  with `assets/sketches/radiance/screenshot.png`.
- `OnEnter` spawn (RadianceRoot marker, chained with audio-input start and camera
  arbitration), `OnExit` `despawn_with::<RadianceRoot>` + `remove_*_resources` +
  `reset_render_profile` + audio-input teardown + hand-provider restore.
- Update systems gated `sketch_active(AppState::Radiance)`.
- Shared generics: `restart_on_settings_change`, `reload_on_resize_settled`,
  `apply_render_profile`, `reset_render_profile`.
- Render-world resources: `ExtractResource` mirrors + explicit
  `remove_*_if_absent` `ExtractSchedule` systems (removals don't auto-propagate).
- Bind-group caches keyed on `BufferId`/`TextureViewId`, bounded by construction.

## Testing

- **POD parity:** `offset_of!` tests for `RadianceSimParams` and any WGSL-mirrored
  struct (particle.rs is the model).
- **Synthetic body performer:** deterministic mask + landmark generator (analog of
  the synthetic sweeping hand) shared by unit tests, the attract phantom, and
  `cargo xtask capture radiance-*` visual-regression scenarios — no camera needed,
  reproducible captures.
- **Audio unit tests:** AGC convergence on step inputs; band energies on synthesized
  tones; spectral-flux onset on a click train; ring drain under buffer pressure.
- **Edge extraction:** known masks (circle, torso blob) → expected point counts,
  normals outward, capacity clamp.
- **Fixture `AudioAnalysis`** values for audio-coupling tests.
- Standard gates: fmt, clippy `-D warnings`, nextest `--all-features`, doc build
  (watch intra-doc links to feature-gated items — doc gate builds default features
  only), `cargo deny`, `cargo xtask check-secrets`.
- Manual: `cargo rund` smoke test with a live camera + mic (prompt Madison; note
  the Dev knobs sit behind the ADVANCED toggle).

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| BlazePose loses track on very fast motion | Detector re-run is cheap; mask EMA bridges gaps; MoveNet Lightning documented as landmark fallback |
| 256×256 mask is soft/impressionistic at distance | Treated as aesthetic (aura, not cutout); rim + fill tuned for it; RVM upgrade rejected on license |
| CoreML partition explosion on pose models | Known failure mode with a written playbook (`onnx-coreml-model-surgery.md`) + per-model cache keys |
| Mic signal wildly variable at a party | AGC by design; sensitivity setting; sketch degrades to motion-only drive |
| Thermal budget over multi-hour runs | Curl-noise particles (no fluid solve); dispatch scales with settings; idle 4 Hz detector; screensaver present-rate throttle |
| Camera contention with MediaPipe hands | Explicit arbitration on enter/exit; Leap unaffected |
| Disk: second worktree can't hold a second 33 GB `target/` | Operational, not design: free space before Plan A implementation begins |

## Follow-ups explicitly deferred

- Multi-person tracking and mask compositing.
- True fluid velocity field replacing curl noise (the emission/audio machinery is
  deliberately field-agnostic).
- WASM path via a websocket body provider.
- Audio-reactive attract mode.
