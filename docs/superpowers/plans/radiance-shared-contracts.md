# Radiance cross-plan interface contracts (pinned 2026-07-12)

These shapes are FIXED across the three plans. A plan may add fields/methods but must not
rename or retype anything listed here. All paths relative to repo root (worktree
`.worktrees/radiance`, branch `radiance`, based on v5-alpha).

## Plan A produces (audio input + analysis)

Module: `crates/wc-core/src/audio/input/` (mod.rs + capture.rs + analysis.rs + devices.rs)

```rust
/// Always present once AudioInputPlugin is added; neutral values when inactive.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct AudioAnalysis {
    pub rms: f32,             // post-AGC smoothed level, ~0..1
    pub gain: f32,            // current AGC gain multiplier (1.0 when neutral)
    pub bands: [f32; 8],      // log-spaced band energies, post-AGC, ~0..1
    pub onset: f32,           // spectral-flux onset strength this frame, >= 0
    pub beat_confidence: f32, // 0..1 debounced beat estimate
    pub active: bool,         // capture stream healthy and producing samples
}

/// Activation contract: INSERT this resource to start capture; REMOVE it to stop.
/// Sketch-agnostic: Plan C inserts OnEnter(Radiance) / removes OnExit.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct AudioCaptureRequest {
    pub device_name: Option<String>, // None => system default input device
    pub paused: bool,                // true during Idle/Screensaver: stream paused, analysis neutral
}

/// Runtime-enum source for the device dropdown.
#[derive(Resource, Default)]
pub struct AvailableAudioInputDevices(pub Vec<String>);
// registered with OPTIONS_KEY = "audio_input_devices"
```

`AudioInputPlugin` is added by the core audio plumbing (alongside AudioPlugin), NOT by a sketch.

## Plan B produces (body tracking)

Module: `crates/wc-core/src/input/body/` (+ capture promotion:
`input/providers/mediapipe/capture/` moves to `input/capture/`, re-exported so the
mediapipe hand provider keeps compiling).

```rust
pub const BODY_LANDMARK_COUNT: usize = 33;
pub const MAX_EDGE_POINTS: usize = 2048;
pub const MASK_SIZE: usize = 256; // mask is 256x256 R8Unorm

/// Activation contract: INSERT to start the worker+camera; REMOVE to stop.
/// Plan C inserts OnEnter(Radiance) / removes OnExit.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct BodyTrackingRequest {
    pub idle_throttle: bool, // true during Idle/Screensaver: detector-only at idle rate
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BodyLandmark {
    pub pos: Vec3,        // x,y screen-normalized 0..1 (mask UV space), z relative depth
    pub visibility: f32,  // 0..1
}

/// Always present once BodyTrackingPlugin is added; `present == false` when no request/no person.
#[derive(Resource, Clone, Debug)]
pub struct BodyTrackingState {
    pub present: bool,
    pub confidence: f32,
    pub landmarks: [BodyLandmark; BODY_LANDMARK_COUNT],
    pub world_landmarks: [Vec3; BODY_LANDMARK_COUNT], // metric, One-Euro smoothed
    pub velocities: [Vec3; BODY_LANDMARK_COUNT],      // screen-normalized units/sec, smoothed
    pub timestamp: Duration,
}

/// 256x256 R8Unorm image, EMA-smoothed person mask, written in place each body frame.
#[derive(Resource, Clone)]
pub struct MaskTexture(pub Handle<Image>);

/// CPU edge list extracted where the smoothed mask crosses 0.5.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct EdgePoint {
    pub pos: Vec2,    // mask UV space 0..1
    pub normal: Vec2, // outward unit normal
}

#[derive(Resource)]
pub struct SilhouetteEdges {
    pub points: Vec<EdgePoint>, // capacity MAX_EDGE_POINTS, refilled in place (clear(), never realloc)
    pub generation: u64,        // bumped on each new body frame (lets consumers skip re-upload)
}
```

MediaPipe pose landmark indices (subset Plan C uses for impulses): nose=0, left_wrist=15,
right_wrist=16, left_hip=23, right_hip=24, left_ankle=27, right_ankle=28.

Presence: while `BodyTrackingRequest` exists, a person-bearing body frame resets the existing
`InteractionTimer` (same semantics as hand-bearing frames in `reset_on_interaction`).

## Plan C consumes

Everything above, verbatim. Plan C owns: `AppState::Radiance`, `RadianceSettings`
(storage key "radiance"), the sketch module `crates/wc-sketches/src/radiance/`,
shaders `assets/shaders/radiance/`, camera arbitration vs the MediaPipe hand provider
(existing registry/selection APIs), screensaver phantom (synthetic mask + edges fed
through `SilhouetteEdges`/`MaskTexture` by writing the same resources), and the
insert/remove of `AudioCaptureRequest` + `BodyTrackingRequest` (with `paused`/
`idle_throttle` driven by `SketchActivity`).
