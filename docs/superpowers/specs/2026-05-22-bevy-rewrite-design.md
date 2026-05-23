# WaveConductor v5: Rust + Bevy Rewrite — Design Spec

**Status:** Draft, awaiting implementation plan
**Date:** 2026-05-22
**Branch:** `rewrite/bevy`
**Version target:** `v5.0.0`

---

## 1. Summary

Rewrite WaveConductor from the ground up in Rust on the Bevy game engine. Drop Electron, React, Three.js, the Ultraleap WebSocket bridge, and `leapjs`. Preserve every shipped feature behaviorally, target multi-hour unattended thermal stability as the primary goal, keep web as a first-class deployment target, and lay an architecture that supports future input providers (MediaPipe) without disturbing sketches.

## 2. Goals and non-goals

### Goals

- **Multi-hour unattended thermal stability.** v5 must run for at least 8 hours on the deployment hardware with bounded resource growth and no manual intervention.
- **No lost features.** Every sketch, every settings field, every keyboard shortcut, every UI affordance present in v4 ships in v5.
- **Idiomatic Rust + Bevy.** Workspace structure, ECS + States patterns, plugin-per-subsystem, derive macros for typed configuration, `Send + Sync` discipline.
- **First-class native and web targets.** macOS DMG, Windows portable `.exe`, Linux AppImage, and a WebAssembly build deployed to GitHub Pages from the same workspace.
- **Future-proof input layer.** Mouse, touch, and Leap on day one; MediaPipe slots in later behind the same trait with no sketch changes.
- **Measurable performance improvement.** A reproducible v4-vs-v5 benchmark harness produces a documented improvement report before v5.0 release.
- **Strict coding standards** enforced in CI: format, lint, doc, secret-scan, dependency policy, audit, soak.

### Non-goals

- **Backward-compatible settings.** v4 settings do not migrate. v5 ships with defaults across all platforms.
- **Pixel-identical sketch output.** Each sketch declares its own parity bar (perceptual, reinterpreted, or physics-matched) in `PARITY.md`.
- **v4 maintenance after cutover.** Once v5.0.0 ships, v4 binaries remain available on the releases page but receive no updates.
- **Auto-launch on system startup.** Out of scope for v5.0; a user/installer concern.

## 3. Context

v4 is a React 19 + TypeScript + Vite + Three.js + Web Audio app packaged as an Electron desktop binary, with a parallel browser build deployed to GitHub Pages. Five sketches (line, flame, dots, cymatics, waves), per-sketch settings stored in `localStorage`, a screensaver after 30 seconds idle, mouse + touch + Leap Motion input via a bundled WebSocket bridge.

The motivating problem is that the v4 stack pins CPU during sketch idle periods enough to trigger thermal throttling on a MacBook Pro after several hours unattended. Optimizations within the existing stack have been exhausted. The fix is architectural: leave the JS + WebView stack entirely.

Bevy was chosen after a parallel evaluation of Bevy, raw wgpu, Nannou, and Macroquad. Bevy wins on ecosystem health, scheduler-level idle gating via `States`, modern shader stack (WGSL + compute), and library maturity (`bevy-egui`, `bevy_mod_debugdump`, growing creative-coding adjacency).

## 4. Architecture overview

A Cargo workspace with three crates:

- `crates/waveconductor` — binary crate. Window creation, `App` setup, plugin registration, OS-specific entry points, bundling.
- `crates/wc-core` — shared library. Input trait and providers, audio engine, settings store, lifecycle (states, idle, screensaver), math, UI primitives.
- `crates/wc-sketches` — plugin set. Five sketches as Bevy `Plugin`s, one module per sketch.

A separate `perf-harness/` crate (workspace member, never shipped with the app) runs v4-vs-v5 benchmarks.

Cross-cutting:

- Sketch selection is a Bevy `States` enum. Switching a sketch is a state transition; inactive sketches have zero systems running.
- Idle and screensaver are sub-states gating per-frame work at the scheduler level.
- Input is a trait, with five concrete providers (mouse, touch, native Leap via `leaprs`, WebSocket Leap for web/dev, mock for tests). MediaPipe is a planned future provider.
- Audio runs on a dedicated thread driven by `cpal`. DSP synthesis via `fundsp`. Analysis via `rustfft`. The Bevy main thread communicates with audio over lock-free ring buffers.
- Settings are typed Rust structs per sketch. A derive macro emits the runtime metadata egui needs. Persisted as TOML on native, `localStorage` on web.
- Web build uses wgpu's WebGPU path with WebGL2 fallback. Audio via cpal's `web_sys` backend. Leap via WebSocket fallback. Bundle ~15–30 MB, accepted.

## 5. Detailed design

### 5.1 Workspace layout and file organization

```
waveconductor/                                # repo root
├── Cargo.toml                                # workspace manifest, shared lints, profiles
├── Cargo.lock
├── rust-toolchain.toml                       # pinned stable channel
├── CLAUDE.md                                 # one-line: @AGENTS.md
├── AGENTS.md                                 # coding standards (see §6)
├── README.md
├── LICENSE
├── deny.toml                                 # cargo-deny config
├── rustfmt.toml
├── clippy.toml
├── .github/workflows/
│   ├── ci.yml                                # fmt, clippy, deny, audit, tests, all targets
│   ├── release.yml                           # native artifacts: DMG, portable exe, AppImage
│   └── deploy-web.yml                        # wasm build → GitHub Pages
├── assets/                                   # shaders, fonts, images, audio samples
│   ├── shaders/                              # WGSL files, hot-reloadable in dev
│   ├── fonts/
│   └── icons/
├── crates/
│   ├── waveconductor/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs                       # window, App, plugin registration
│   │       ├── platform/
│   │       │   ├── mod.rs
│   │       │   ├── native.rs                 # cfg(not(target_arch = "wasm32"))
│   │       │   └── web.rs                    # cfg(target_arch = "wasm32")
│   │       └── build.rs                      # bundle metadata
│   ├── wc-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs                        # CorePlugin
│   │       ├── audio/                        # cpal + fundsp + FFT, ring buffers
│   │       ├── input/                        # HandTrackingProvider trait + pointer merge
│   │       │   ├── mod.rs                    # trait, HandTrackingState, HandGestureEvent
│   │       │   ├── pointer.rs                # PointerState + pointer_merge_system
│   │       │   ├── leap/                     # native LeapC via `leaprs`, cfg-gated
│   │       │   ├── websocket/                # web target + native dev fallback
│   │       │   └── mock.rs                   # test-only
│   │       ├── settings/                     # SettingDef, derive macro, store
│   │       ├── lifecycle/                    # AppState, SketchActivity, idle, screensaver
│   │       ├── math/                         # noise (via `noise` crate), tuning; easing is bevy_math::curve native
│   │       └── ui/                           # bevy-egui panels, status widgets
│   └── wc-sketches/
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs                        # SketchesPlugin: registers all 5
│       │   ├── line/                         # one module per sketch
│       │   │   ├── mod.rs                    # Plugin impl, settings struct
│       │   │   ├── systems.rs                # update + cleanup systems
│       │   │   ├── shaders.rs                # WGSL asset handles
│       │   │   └── PARITY.md                 # per-sketch parity decision and notes
│       │   ├── flame/
│       │   ├── dots/
│       │   ├── cymatics/
│       │   └── waves/
│       └── tests/                            # integration tests (per Rust convention)
│           ├── line_lifecycle.rs
│           ├── flame_lifecycle.rs
│           ├── dots_lifecycle.rs
│           ├── cymatics_lifecycle.rs
│           └── waves_lifecycle.rs
├── perf-harness/                             # v4-vs-v5 benchmarks, not shipped
│   ├── Cargo.toml
│   ├── src/
│   └── scenarios/                            # *.toml input replay scripts
├── xtask/                                    # dispatcher: release, check-secrets, soak-test, perf-audit
│   ├── Cargo.toml
│   └── src/
└── docs/
    ├── superpowers/specs/                    # design docs (this file)
    ├── adr/                                  # short architecture decision records
    └── perf-audits/YYYY-MM-DD/               # benchmark outputs
```

Workspace `Cargo.toml` carries shared `[workspace.lints]` (clippy `pedantic` plus a curated `restriction` subset), shared `[profile.release]` (LTO fat, `codegen-units = 1`, `panic = "abort"`), and pinned dependency versions via `[workspace.dependencies]`.

### 5.2 Lifecycle: AppState and SketchActivity

```rust
#[derive(States, Default, Clone, Eq, PartialEq, Hash, Debug)]
pub enum AppState {
    #[default]
    Home,
    Line,
    Flame,
    Dots,
    Cymatics,
    Waves,
}

#[derive(SubStates, Clone, Eq, PartialEq, Hash, Debug)]
#[source(AppState = AppState::Line | AppState::Flame | AppState::Dots
                  | AppState::Cymatics | AppState::Waves)]
pub enum SketchActivity {
    Active,
    Idle,
    Screensaver,
}
```

Each sketch plugin registers:

- `init` system on `OnEnter(AppState::Foo)`. Spawns entities, builds DSP graph, sets up GPU resources, all tagged with a sketch marker component.
- `update` system on `Update.run_if(in_state(AppState::Foo).and(in_state(SketchActivity::Active)))`. Reads Bevy native input resources and `HandTrackingState`, mutates components, writes audio params, updates uniforms.
- `cleanup` system on `OnExit(AppState::Foo)`. Despawns by marker, releases GPU resources.

Interaction from any source (mouse motion, mouse/keyboard button, touch, or hand presence in `HandTrackingState`) resets an `InteractionTimer` resource. A `lifecycle` system in `wc-core` transitions `SketchActivity::Active → Idle` after the configured timeout (default 30 s) and shows the screensaver overlay after a second timeout. When idle, sketch update systems do not run; only interaction watchers do.

This makes the per-sketch idle hardening you implemented in v4 a scheduler invariant in v5. Verified in integration tests by inspecting the schedule with `bevy_mod_debugdump`.

Per-sketch parity decisions live in `crates/wc-sketches/src/<sketch>/PARITY.md`, declaring the parity target (perceptual, physics-matched, or reinterpreted), reference media commit hashes, and any approved deviations.

### 5.3 Input

Mouse, touch, and keyboard are handled by Bevy's native input resources directly. No adapter layer. Sketches consume:

- `Res<ButtonInput<MouseButton>>` — `pressed` / `just_pressed` / `just_released` per button, O(1) lookup, window-focus aware.
- `Res<AccumulatedMouseMotion>`, `Res<AccumulatedMouseScroll>` — per-frame accumulated deltas.
- `Events<MouseMotion>`, `Events<MouseWheel>`, `Events<MouseButtonInput>` — raw events via `MessageReader` when needed.
- `Res<Touches>` — full active-touch tracker (`iter`, `iter_just_pressed`, `iter_just_released`, per-touch `position()`, `id()`).
- `Res<ButtonInput<KeyCode>>` for keyboard, plus run-condition helpers like `input_just_pressed(KeyCode::Escape)` for declarative system gating.
- Window cursor position via `window.cursor_position()`.

This is the idiomatic Bevy way and matches how every Bevy app reads mouse, touch, and keyboard. Wrapping these in a custom adapter is pure ceremony — they are already abstracted over the OS input backends by Bevy.

The only input modality Bevy does not natively know about is hand tracking. We add it the same way Bevy adds its own input modalities: a `HandTrackingPlugin` modeled exactly on `InputPlugin`, exposing the same shape of resources and events that sketches already use for mouse and touch.

```rust
pub struct HandTrackingPlugin;

impl Plugin for HandTrackingPlugin {
    fn build(&self, app: &mut App) {
        app
            .init_resource::<HandTrackingState>()       // continuous, like Touches
            .init_resource::<ButtonInput<HandButton>>() // discrete press state, like ButtonInput<MouseButton>
            .add_event::<HandTrackingFrame>()           // raw provider output
            .add_event::<HandGestureEvent>()            // derived discrete moments
            .add_systems(
                PreUpdate,
                (
                    poll_active_provider,
                    update_hand_tracking_state,
                    detect_gestures,
                )
                    .chain()
                    .in_set(InputSystems),              // ride Bevy's existing ordering label
            );
    }
}
```

What sketches consume:

- `Res<HandTrackingState>` — continuous per-hand data (active hand count, 21-landmark per-hand data, palm normal, pinch strength, grab strength, timestamp). Same idiom as `Res<Touches>`.
- `Res<ButtonInput<HandButton>>` where `HandButton ∈ {LeftPinch, RightPinch, LeftGrab, RightGrab, …}` — gives sketches `pinch.just_pressed(HandButton::LeftPinch)` with the exact same shape as `mouse.just_pressed(MouseButton::Left)`.
- `Events<HandGestureEvent>` — discrete moments worth eventing (swipe, double-pinch).
- `Events<HandTrackingFrame>` — raw provider frames for systems that want them (analytics, recording).

Behind the plugin, a strategy-pattern trait selects which source feeds the pipeline:

```rust
pub trait HandTrackingProvider: Send + Sync + 'static {
    fn start(&mut self) -> Result<(), HandTrackingError>;
    fn stop(&mut self);
    fn poll(&mut self, out: &mut Events<HandTrackingFrame>);
    fn status(&self) -> HandTrackingStatus;
}
```

App startup picks one provider and installs it as a resource; everything downstream is invisible to sketch code. Sketches never reference the trait. `poll_active_provider` calls `poll()` once per frame in `PreUpdate`; `update_hand_tracking_state` consumes those raw frames into `HandTrackingState` + `ButtonInput<HandButton>`; `detect_gestures` turns frame-to-frame transitions into `HandGestureEvent`s.

Providers in v5.0:

- `LeaprsProvider` — native only (`cfg(not(target_arch = "wasm32"))`), links LeapC via the `leaprs` crate. Direct device access. Replaces the `Ultraleap-Tracking-WS` binary, the WebSocket bridge, the port-6437 polling, and the IPC spawn/exit dance in `electron/main.ts`. Single process, microsecond latency.
- `WebSocketProvider` — web target plus native dev fallback. Speaks the existing Ultraleap WS protocol on port 6437 so the existing external WS server keeps working on the web build.
- `MockProvider` — test only. Plays back recorded `HandFrame` sequences for integration tests and perf-harness scenarios.

Future:

- `MediaPipeProvider` — webcam-based hand tracking. Native via `ort` (ONNX Runtime) + MediaPipe Hands ONNX export. Web via JS interop with `@mediapipe/tasks-vision`, landmarks passed across the WASM boundary. Behind a Cargo feature flag.

Provider selection is a startup decision read from app-level configuration (separate from per-sketch settings). One hand-tracking provider runs at a time.

#### Optional unified pointer

For sketches that want a single "wherever the user is pointing" stream across mouse, touch, and hand (the way v4's `BaseSketch.events` blended them), a thin `pointer_merge_system` writes a `Res<PointerState>` derived from Bevy's native input resources plus `HandTrackingState`. This is a merge convenience, not a provider abstraction. Sketches that only care about mouse read Bevy's native resources directly.

A `wc-core/ui/` egui widget renders the `LeapStatusIndicator` equivalent, reading `HandTrackingStatus` from the active provider. The widget is the v4 indicator at feature parity.

#### Action-shaped input via `leafwing-input-manager`

All *discrete, action-shaped* input across every modality (keyboard, mouse buttons, touch presence, hand buttons) is unified through `leafwing-input-manager`. *Continuous positional* and *structured* data (cursor position, hand landmarks, audio spectrum) stays on Bevy-native resources — leafwing is not designed for those and we don't bend it to fit. The dividing line:

| Input shape                                                | Read from                                                                              |
| ---------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| Discrete action state ("pressed", "just pressed", "just released") | `Res<ActionState<T>>` (leafwing)                                                       |
| Continuous positional data (cursor, touches, palm in screen space) | Bevy native (`window.cursor_position()`, `Res<Touches>`) + `Res<PointerState>` merge   |
| Continuous structured state (hand landmarks, FFT bins, palm normal) | `Res<HandTrackingState>`, `Res<AudioState>`                                            |

**App-level shortcuts** are one global `WaveConductorAction` enum bound by leafwing:

```rust
#[derive(Actionlike, Reflect, Clone, Hash, PartialEq, Eq, Debug)]
enum WaveConductorAction {
    NavigatePrev, NavigateNext, SelectSketch(u8),
    ToggleVolume, ToggleDevPanel, ToggleFullscreen, Quit,
}
```

v4's hotkey table (`1`–`5`, `z`/`←`, `x`/`→`, `Escape`, `V`, `Shift+D`, `F11`, `Alt+F4`) becomes the `InputMap<WaveConductorAction>` bindings. The lifecycle plugin and UI systems read `Res<ActionState<WaveConductorAction>>` with the same `pressed` / `just_pressed` idioms as `ButtonInput`.

**Per-sketch interactions** are per-sketch action enums, each independently bound and cross-modal:

```rust
#[derive(Actionlike, Reflect, Clone, Hash, PartialEq, Eq, Debug)]
enum LineAction {
    SpawnAttractor,    // bound to MouseButton::Left, any TouchInput active,
                       // HandButton::LeftPinch, HandButton::RightPinch
}
```

A sketch system then reads action and position as two separate questions:

```rust
fn line_update(
    action_state: Res<ActionState<LineAction>>,
    pointer: Res<PointerState>,
    // ...
) {
    if action_state.pressed(&LineAction::SpawnAttractor) {
        spawn_attractor_at(pointer.primary);
    }
}
```

The sketch asks "is the user trying to attract?" (action) and "where are they pointing?" (position). It does not know or care which physical input fired the action.

**Integration**: `HandButton` implements leafwing's `Buttonlike` trait in `wc-core/input/` so it can appear in `InputMap`. `HandTrackingPlugin` continues to populate `Res<ButtonInput<HandButton>>` (the Bevy-native resource) *and* drives leafwing's `ActionState` through the trait. Future `MediaPipeProvider` slots in with zero leafwing-side changes — same `HandButton` enum, different driver underneath. Future "rebind controls" UI gets built once and covers keyboard, mouse, and hand uniformly.

### 5.4 Audio

Audio runs off the Bevy thread. Two-way communication via lock-free ring buffers and an `AudioState` resource.

```
┌──────────────────────────┐        ┌─────────────────────────────┐
│ Bevy main thread (60 Hz) │        │ Audio thread (cpal callback)│
│                          │        │                             │
│  Sketch system           │        │  Mix sources                │
│   ↓ (write sketch params)│        │   ↑ (read params snapshot)  │
│  audio_param_ring  ──────┼───────►│                             │
│                          │        │  Synthesize via fundsp graph│
│                          │        │   ↓                         │
│  AudioState resource     │◄───────┼── analysis_ring             │
│  (FFT bins, levels)      │        │  (FFT every N samples)      │
│   ↑                      │        │   ↓                         │
│  Shader uniform system   │        │  Output samples → cpal      │
└──────────────────────────┘        └─────────────────────────────┘
```

Crates:

- `cpal` — cross-platform audio I/O. Native backends (CoreAudio, WASAPI, ALSA) plus `web_sys` backend for WASM.
- `fundsp` — DSP graph for per-sketch synthesis. Each audio-generative sketch builds its graph in its `init` system and updates parameters per frame through the param ring buffer.
- `rustfft` — FFT for analysis. The Waves sketch and any future audio-reactive features read `AudioState.spectrum_bins`.
- `rtrb` — single-producer single-consumer ring buffers for thread crossings.

`AudioPlugin` (in `wc-core/audio/`) initializes cpal streams at app start, spawns the audio thread with the DSP graph host, and exposes the `AudioState` resource. A per-sketch `AudioHandle` component is spawned by sketch init systems; sketches push parameter updates through it.

`bevy_audio` is not used. Its API is one-shot SFX and does not support the streaming synthesis model.

### 5.5 Settings

Each sketch declares a typed settings struct:

```rust
#[derive(SketchSettings)]
pub struct LineSettings {
    #[setting(default = 5000, min = 100, max = 50000, requires_restart, category = User)]
    pub particle_count: u32,

    #[setting(default = 0.92, min = 0.5, max = 1.0, step = 0.01, category = Dev)]
    pub attractor_decay: f32,

    #[setting(default = "#ffffff", category = User, ty = Color)]
    pub line_color: [f32; 4],
}
```

The `SketchSettings` derive macro emits:

- `Default` impl based on the per-field `default` annotation.
- `serde::Serialize` and `serde::Deserialize` impls.
- `bevy_reflect::Reflect` impl, so settings are inspectable by any Reflect-based tool (including `bevy-inspector-egui`).
- A runtime `SettingsDef` table (label, min, max, step, category, type hint, requires-restart flag) consumed by the egui panel.

Persistence:

- **Native**: TOML at `dirs::config_dir() / "waveconductor" / "sketch-settings.toml"`. Loaded on startup, written on change with debounce.
- **Web**: `web-sys` `localStorage`, one key per sketch.

No v4 migration. v5 ships with defaults on every platform on first launch.

Panels:

- **User panel** — a `bevy-egui` window with curated labels and grouping. Renders only `category = User` fields. Writes back through the typed struct.
- **Dev panel (Shift+D)** — uses `bevy-inspector-egui` to introspect any Reflect-derived settings resource (plus any other inspectable Bevy resource we choose to expose). Replaces the v4 ad-hoc dev panel with a strictly better tool. No custom UI to maintain.

A `requires_restart` change fires a `SketchRestart` event; the lifecycle plugin transitions `OnExit → OnEnter` on the active sketch.

### 5.6 Web build

- **Renderer**: wgpu auto-selects WebGPU where available, WebGL2 fallback otherwise. WebGL2 means no compute shaders. Particle sketches (Line, Dots, Flame) ship a CPU-side fallback path selected at startup by a runtime capability check on `Features::COMPUTE_SHADER`. Fragment-shader-only sketches (Cymatics, Waves) are unaffected.
- **Bundle**: Bevy WASM is ~15–30 MB compressed. Accepted cost. `wasm-opt -Oz` in the release pipeline; gzip and brotli served from GitHub Pages.
- **Audio**: cpal's WASM backend uses `web_sys` `AudioContext`. `AudioPlugin` API is identical across targets.
- **Input on web**: `WebSocketProvider` for Leap (existing Ultraleap WS server continues to work). Mouse, touch, and keyboard are Bevy native and always available on web. Mobile web is touch + keyboard only.
- **Hot reload**: dev iteration on web uses `trunk serve` with WGSL hot reload. Native dev uses `cargo run` with `bevy::asset::AssetPlugin::watch_for_changes` enabled.
- **Routing**: a small JS shim on startup reads `window.location.hash`; a Bevy system maps the hash to `AppState`. Same UX as the v4 HashRouter.

### 5.7 Distribution and release

Targets:

- **macOS universal DMG** — `cargo-bundle` or `cargo-packager`, code-signed with Developer ID Application, notarized in CI. Universal binary (arm64 + x86_64).
- **Windows portable `.exe`** — cross-compiled from macOS via `cargo-xwin` in CI, or built natively on a Windows runner. Self-signed; SmartScreen behavior matches v4.
- **Linux AppImage** — free byproduct of Bevy + winit. Shipped at no ongoing maintenance cost.
- **Web** — `trunk build --release` → GitHub Pages on push to `main`.

Release flow:

1. Bump `version` in workspace `Cargo.toml` and commit.
2. `cargo xtask release` tags `v5.x.y` and pushes.
3. GitHub Actions builds all four artifacts and creates a draft release.
4. Review, edit notes, publish.

Kiosk requirements port verbatim:

- Fullscreen: `WindowMode::BorderlessFullscreen`.
- Display sleep prevention: `keepawake-rs` (cross-platform: `IOPMAssertion` on macOS, `SetThreadExecutionState` on Windows).

### 5.8 Testing strategy

Four layers, each with a clear bar.

**Unit tests** — pure functions, colocated `#[cfg(test)] mod tests`. Coverage target 100% on `wc-core/math`, `wc-core/settings/store`, `wc-core/input` (event normalization, pointer projection), `wc-core/audio` (FFT processing, ring buffer logic), per-sketch physics modules. These port one-for-one from v4's existing test suite.

**Integration tests** — Bevy app harness using `MinimalPlugins`. Each sketch crate ships at least:

- "init then exit doesn't leak entities" — count entities before and after.
- "100-frame run with `MockProvider` doesn't panic."
- "settings round-trip preserves values."
- "switching between sketches doesn't leave systems running" — inspect schedule with `bevy_mod_debugdump`.

**Visual review** — manual side-by-side video against each sketch's parity bar. A `cargo xtask parity-capture` command launches each sketch with a fixed seed and `MockProvider` playing a recorded gesture sequence, captures 10 seconds of frames to MP4. Verdict is recorded in each sketch's `PARITY.md`. No golden-image diffing.

**Soak test** — `cargo xtask soak-test --duration 8h --sketch <name>` runs unattended on deployment hardware. Logs hourly: RSS, GPU memory, CPU sample. Required before any release tag. Pass criteria: RSS growth less than 2× baseline, no crashes, no GPU resource exhaustion.

Tooling: `cargo nextest`, `cargo llvm-cov`, `bevy_mod_debugdump`, `insta` for snapshot tests.

### 5.9 Performance audit harness

A `perf-harness/` crate that runs reproducible v4-vs-v5 benchmarks before v5.0 release and again on every subsequent minor.

```
cargo xtask perf-audit --scenario <name> [--target macos|windows|linux] \
    [--report-dir docs/perf-audits/YYYY-MM-DD/]
```

The harness:

1. Launches v4 (Electron build) and v5 (native binary or web build) sequentially on the same machine.
2. Drives both with the same scripted input scenario, e.g. `scenarios/festival-loop.toml`: home for 10 s → Line for 60 s with recorded Leap track → Cymatics for 60 s → idle for 5 min → Line for 60 s. v5 replays via `MockProvider`. v4 replays via a "perf mode" added to `electron/main.ts` ahead of cutover.
3. Samples metrics every 1 s during the run. The v5 side uses Bevy's built-in diagnostic plugins — `FrameTimeDiagnosticsPlugin` (frame timing), `EntityCountDiagnosticsPlugin` (entity count growth), `SystemInformationDiagnosticsPlugin` (CPU%, RSS) — exposed as `Res<DiagnosticsStore>` and sampled by the perf-harness driver. The v4 side uses `stats.js` injected into the renderer plus OS-level sampling. OS-level metrics common to both runs: package temperature (`powermetrics` on macOS behind `--with-thermal`, `OpenHardwareMonitor` on Windows, `lm-sensors` on Linux), GPU memory and utilization, audio output latency (RMS-to-RMS via a loopback device when available), energy and power draw.
4. Generates a report:
   - `report.md` — human-readable, verdict per metric per phase.
   - `metrics.csv` — raw samples for downstream analysis.
   - `chart.svg` — overlaid v4 and v5 time series.
   - `scenario.toml` — exact input replay used.

Cadence:

- Captured periodically on `rewrite/bevy` during development; results committed to `docs/perf-audits/`.
- A v5.0 launch audit runs on deployment hardware under the festival-loop scenario; result lives in release notes.
- Subsequent minor releases re-run the same audit; regressions become release blockers.

Honest caveats documented in the report template: Electron and Bevy use different telemetry surfaces; thermal data depends on ambient conditions; this is engineering instrumentation, not academic benchmarking.

### 5.10 Linting and quality gates

CI runs on every PR and push:

| Gate              | Command                                                                | Blocks merge |
| ----------------- | ---------------------------------------------------------------------- | ------------ |
| Format            | `cargo fmt --all -- --check`                                           | yes          |
| Lint              | `cargo clippy --all-targets --all-features -- -D warnings`             | yes          |
| Tests             | `cargo nextest run --all-features`                                     | yes          |
| Native build      | `cargo build --release` per target                                     | yes          |
| Web build         | `trunk build --release`                                                | yes          |
| Coverage          | `cargo llvm-cov --fail-under-lines 80`                                 | yes          |
| Dependency policy | `cargo deny check`                                                     | yes          |
| Security audit    | `cargo audit --deny warnings`                                          | yes          |
| Secret scan       | `cargo xtask check-secrets`                                            | yes          |
| Doc build         | `cargo doc --no-deps --workspace --document-private-items` (deny warn) | yes          |

`[workspace.lints]` enables clippy `pedantic` plus a curated `restriction` subset (`unwrap_used`, `expect_used`, `panic`, `as_conversions`). Local `#[allow(...)]` requires a `// reason: ...` comment.

`rustfmt.toml`: defaults plus `imports_granularity = "Crate"`, `group_imports = "StdExternalCrate"`.

#### Dispatcher: `cargo xtask`

A single dispatcher script under `xtask/` (its own workspace member) provides the agent-friendly entry points referenced throughout this spec. All subcommands accept `--json` for machine-readable output.

| Subcommand        | Purpose                                                                                                |
| ----------------- | ------------------------------------------------------------------------------------------------------ |
| `release`         | Bump version, tag `v5.x.y`, push.                                                                      |
| `check-secrets`   | Regex scan for home-directory paths, email patterns, common secret prefixes. CI-blocking on match.     |
| `parity-capture`  | Run each sketch with a fixed seed and `MockProvider` script, capture 10 s of frames to MP4.            |
| `soak-test`       | Run a sketch unattended for N hours, logging hourly RSS / GPU memory / CPU.                            |
| `perf-audit`      | Drive v4 and v5 through a scenario, sample platform metrics, generate report.                          |
| `manifest`        | List all subcommands with descriptions (CLI self-documentation).                                       |

### 5.11 Branch and migration strategy

- Development on a `rewrite/bevy` branch from `main`.
- v4 stays on `main` until v5.0.0 is feature-complete and parity-validated per sketch.
- When v5.0.0 ships: squash-merge `rewrite/bevy` into `main`. Tag `v4-final` on the pre-merge `main` so v4 sources are recoverable.
- v4 binaries remain on the releases page (versioned `v4.x.y`); they are not removed.
- No settings migration; v5 ships with defaults on every platform.
- `bin/Ultraleap-Tracking-WS-*`, `scripts/leap-websocket.ts`, and the `electron/` directory delete during the rewrite. The external `UltraleapTrackingWebSocket` repo continues to exist independently for web users.

### 5.12 Bevy-native and third-party plugin policy

**Defer to Bevy-native APIs whenever they exist.** Custom code is reserved for what Bevy genuinely does not provide. Specifically:

| Concern                                | Use                                                                                 |
| -------------------------------------- | ----------------------------------------------------------------------------------- |
| Mouse, touch, keyboard input           | `Res<ButtonInput<…>>`, `Res<Touches>`, `Res<AccumulatedMouseMotion/Scroll>` (§5.3)  |
| Delta and elapsed time                 | `Res<Time>` — never `Instant::now()` or wall-clock timestamps in systems            |
| Window dimensions and cursor position  | `Single<&Window>` reads; `window.cursor_position()` is already window-relative      |
| Easing functions                       | `bevy_math::curve::EasingCurve` + `EaseFunction` (under the `curve` feature)        |
| Schedule labels                        | `PreUpdate`, `Update`, `OnEnter`, `OnExit`, `InputSystems`                          |
| Run conditions                         | `run_if(in_state(...))`, `input_just_pressed(...)`                                  |
| Asset hot reload                       | `AssetPlugin::watch_for_changes` in dev                                             |
| Frame timing / entity counts / sysinfo | `FrameTimeDiagnosticsPlugin`, `EntityCountDiagnosticsPlugin`, `SystemInformationDiagnosticsPlugin` — primary v5 telemetry for the perf harness |
| Runtime reflection                     | `Reflect` derive on settings structs and any other data inspectable at runtime      |

**Mature third-party plugins adopted in v5.0:**

| Plugin                       | Purpose                                                                                              |
| ---------------------------- | ---------------------------------------------------------------------------------------------------- |
| `bevy-egui`                  | Settings UI panels (User category), in-app overlays                                                  |
| `bevy-inspector-egui`        | Dev panel (Shift+D) — Reflect-based inspection of all settings and any other inspectable resource. Replaces the v4 ad-hoc dev panel. |
| `leafwing-input-manager`     | All discrete, action-shaped input across keyboard, mouse buttons, touch presence, and hand buttons. Declarative `ActionState<T>` enums with cross-modal binding (one action can fire from keyboard *or* mouse *or* hand pinch). Used for app-level shortcuts and per-sketch interactions. Continuous positional/structured data stays on Bevy-native resources, not leafwing. |
| `bevy_framepace`             | Deterministic frame pacing for multi-hour thermal stability. Spike during perf-audit; adopt if it helps the thermal goal. |
| `bevy_mod_debugdump`         | Schedule inspection in integration tests (already in §5.8)                                           |

**Considered and not adopted in v5.0:**

| Plugin              | Why not                                                                                                 |
| ------------------- | ------------------------------------------------------------------------------------------------------- |
| `bevy_audio`        | One-shot SFX only; insufficient for real-time DSP synthesis (§5.4)                                      |
| `bevy_kira_audio`   | Game audio engine (mixing, music, SFX); wrong shape for DSP synthesis. `cpal + fundsp + rustfft` is correct. |
| `bevy_hanabi`       | Effect-graph DSL constrains physics. Custom WGSL compute shaders give exact control of `leapAttractorPower` and attractor decay. Per-sketch spike at port time — if a sketch's physics maps cleanly to hanabi, switch. |
| `bevy_persistent`   | Too generic for our per-sketch + per-platform persistence needs. Direct `serde` + `dirs` (native) + `web-sys` (web) is ~30 LOC and exactly right. |
| `bevy_tokio_tasks`  | Not needed in v5.0. Becomes relevant when MediaPipe lands (async ONNX inference); reconsider then.       |

When evaluating future plugins, the bar is: *replaces meaningful custom code, is actively maintained on the Bevy minor we target, has no major-version churn imminent, and has at least one shipping production user.* Plugins that match all four are adopted; plugins that miss one or more are deferred.

## 6. Documentation and coding conventions

### 6.1 CLAUDE.md

A one-line file at the repo root:

```
@AGENTS.md
```

### 6.2 AGENTS.md

Expanded from the v4 doc-only convention into five sections.

**In-code documentation**

- `///` rustdoc on every public item (struct, enum, trait, fn, module).
- Module-level `//!` on every `mod.rs` or module root describing role and data flow.
- Document signal and data flow at plugin entry points (the `build()` method of each `Plugin`), not at every system call site.
- Inline `//` for math, DSP, and shader uniform contracts. Explain what each term in a formula represents.
- Never strip comments during refactors. Update stale comments rather than removing them.

**Code readability**

- One concept per file. Files over ~300 lines or carrying two unrelated responsibilities are split.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- Prefer named structs over tuple structs once a type has more than one semantically meaningful field.
- No `unwrap()` or `expect()` in non-test code unless the panic is documented as an invariant violation.
- No `as` casts on numeric types where `From` / `TryFrom` / `u32::try_from` would work.
- Function bodies fit on one screen; if not, extract.

**File organization**

- One sketch per directory; entry is `mod.rs`, never an inline single file.
- Shaders live in `assets/shaders/<sketch>/<name>.wgsl`. Never inline WGSL strings in Rust.
- Platform-specific code lives in `platform/native.rs` and `platform/web.rs`; portable modules do not contain `cfg` blocks.
- Test files colocated with source as `#[cfg(test)] mod tests`.
- No `src/utils/` or `src/helpers/` dumping grounds. Helpers live with the module that uses them; truly shared helpers go in a named module under `wc-core/`.

**Application performance**

- Default target is multi-hour unattended thermal stability, not peak FPS.
- Sketches must run zero systems when in `SketchActivity::Idle`. Verified by inspecting the schedule with `bevy_mod_debugdump`.
- No allocations in hot paths (per-frame systems, audio callbacks). Pre-allocate buffers, reuse `Vec`s, use `bevy::ecs::system::Local` for scratch state.
- Audio thread is real-time-friendly: lock-free ring buffers only, no `Mutex`, no allocations after init.
- GPU resources: every per-sketch resource is owned by an entity tagged with the sketch's marker component, despawned on `OnExit` to release VRAM.
- Compute shader dispatch sizes scale with settings; do not dispatch unused workgroups.
- An 8-hour soak test is required before any release tag.

**Security and privacy**

- No private personal information in the repo. No real email addresses (use `noreply.github.com` or placeholder), no phone numbers, no API keys, no tokens, no session IDs, no analytics IDs tied to a real account. Secrets go in environment variables loaded at runtime, never committed.
- No hardcoded local paths. No developer-machine-specific home directories (`/Users/<name>/...`, `C:\Users\<name>\...`, `/home/<name>/...`) in source, configs, scripts, CI, or comments. Paths come from workspace-relative literals (`assets/shaders/...`), runtime resolution (`dirs::config_dir()`, `std::env::current_exe()`), or environment variables.
- Pre-commit lint check: `cargo xtask check-secrets` blocks merges that introduce home-directory path patterns, email patterns, or common secret prefixes.
- `.env.example` checked in; `.env` is `.gitignore`d.
- Screenshots in `README.md` or `docs/` are scrubbed of system chrome that exposes usernames or local paths.

## 7. Risks and open questions

| Risk                                                              | Severity | Mitigation                                                                                                                               |
| ----------------------------------------------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Bevy WASM bundle size hurts web UX                                | Medium   | Accept it; `wasm-opt -Oz`, gzip + brotli, lazy-load assets.                                                                              |
| Bevy version churn during the rewrite                             | Low      | Pin via `[workspace.dependencies]`; budget one day per Bevy minor.                                                                       |
| WGSL semantics differ from GLSL ES in subtle ways                 | Medium   | Per-sketch parity capture and review before declaring a sketch done.                                                                     |
| `leaprs` maintenance status                                       | Medium   | Verify before relying on it; fall back to a thin custom LeapC FFI binding if needed.                                                     |
| Cross-compilation for Windows from macOS in CI                    | Low      | `cargo-xwin` is mature; alternative is a Windows runner.                                                                                 |
| v4 perf-mode shim adds maintenance burden to a near-EOL codebase    | Low      | Shim is small (input IPC + start/stop) and lands once, then v4 freezes.                                                                  |
| Particle sketches on web (no compute shader)                      | Medium   | CPU-side fallback path designed up front, behind `cfg` or runtime capability check.                                                      |

Open questions:

- Specific Bevy minor to target for v5.0 (latest stable at branch start; locked thereafter).
- Whether `bevy_hanabi` warrants reconsideration for any sketch versus hand-rolled compute particles. Default plan is hand-rolled; revisit per sketch at port time.
- Whether `bevy_framepace` measurably improves multi-hour thermal behavior. Spike during the perf audit; adopt if it does, skip if free-running already meets the bar.
- Audio sample rate and buffer size defaults per platform; tuned during the audio plugin implementation.
- Whether `leaprs` is current with the Ultraleap SDK we need; fall back to a thin custom LeapC FFI binding if not.

## 8. Appendix: per-sketch parity decisions

Each sketch ships a `PARITY.md` in its module directory before merge. Contents:

- Parity target: `perceptual`, `physics-matched`, or `reinterpreted`.
- Reference media: commit hash of a v4 capture under fixed input.
- Approved deviations: explicit list of acceptable visual or behavioral differences.
- Verdict: signed off when the v5 implementation meets the bar.

Provisional defaults (subject to per-sketch decision at port time):

| Sketch   | Provisional parity | Rationale                                                                                |
| -------- | ------------------ | ---------------------------------------------------------------------------------------- |
| Line     | Perceptual         | Particle character matters; exact trail shape does not.                                  |
| Flame    | Perceptual         | IFS fractal recognizability is what matters; chaotic detail acceptable as drift.         |
| Dots     | Perceptual         | Same as Line.                                                                            |
| Cymatics | Physics-matched    | The visual is the simulation; if the numerics drift, the sketch is wrong.                |
| Waves    | Perceptual         | Audio-reactivity character matters; FFT bin response can vary if it still feels alive.   |

---

**Next step:** invoke the writing-plans skill to produce a detailed implementation plan.
