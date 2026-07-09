# Windows Remediation (v5.0.0-alpha.5) — Design

**Date:** 2026-07-08
**Status:** Approved, pending implementation plan
**Target release:** `v5.0.0-alpha.5`

## 1. Context

Field testing of `v5.0.0-alpha.4` on Windows produced a crash every 5–13 minutes,
plus a cluster of usability defects. The tester's hardware is an AMD Radeon RX
Vega 10 (integrated GCN5 GPU inside a Ryzen APU), driver `23.20.815.6656`, DX12
backend. This is *not* the intended deployment box (a Radeon 780M mini PC), but
it is the same hardware class: an APU where the CPU cores and the iGPU share one
die, one memory pool, and one thermal budget.

Four sessions were captured in the tester's rolling daily log:

| Session start | Uptime | Terminated by |
| --- | --- | --- |
| 22:56:22 | 5 m 04 s | `DeviceLost: Out of memory` |
| 23:12:44 | 4 m 06 s | clean user exit |
| 23:16:57 | 12 m 50 s | `OutOfMemory` allocation cascade |
| 23:36:40 | 6 m 54 s | `DeviceLost: Out of memory` |

Time-to-death tracks wall-clock uptime rather than user activity, which is the
signature of a per-frame leak. The longest session is the one in which the
tester idled on the Home screen and triggered attract mode, both of which fade
the UI chrome and skip the leaking code path.

The `v5.0.0-alpha.3` instant-crash (Flame's compute shader failing FXC
translation with `X3507: 'apply_variation': Not all control paths return a
value`) is already fixed at `assets/shaders/flame/simulate.wgsl:81-85` and is
confirmed resolved by the alpha.4 logs. It is out of scope here.

## 2. Root causes established

### 2.1 GPU memory leak (fatal)

`crates/wc-core/src/ui/blur/callback.rs:359`:

```rust
let bind_group: &'pass _ = Box::leak(Box::new(bind_group));
```

The comment above it asserts that leaking the Rust handle is safe because "the
GPU resource itself is reference-counted internally by wgpu". This is inverted.
A wgpu `BindGroup` *is* one of those references. `Box::leak` guarantees its
refcount never reaches zero, so the bind group is never freed — and because it
is constructed from `buffer.as_entire_binding()` (`callback.rs:345`), it also
pins the `backdrop_blur_composite_uniforms` buffer allocated fresh on
`callback.rs:313` that same frame.

This executes once per frosted widget per frame. At 60 fps with two or three
widgets visible, that is on the order of ten thousand permanently pinned bind
groups and uniform buffers per minute, plus their descriptor-heap slots.

It also violates the AGENTS.md rule "Never allocate in a hot path."

The `backdrop_blur_uniforms` and `backdrop_blur_down_*` / `_up_*` allocations
named first in the crash log are **victims, not causes**: they are correctly
transient per-frame allocations in `blur/node.rs`, and the blur node runs before
the egui pass, so they are simply the first allocations attempted each frame
once the address space is saturated.

The leak is **not Windows-specific**. There is no `cfg` gate. It leaks on macOS
identically; the Vega 10 merely exhausts its shared-memory carve-out in minutes
rather than hours. It would fail the 8-hour soak on any platform.

### 2.2 Unbounded post-process bind-group caches (contributing)

`crates/wc-sketches/src/line/post_process.rs:322` and
`crates/wc-sketches/src/dots/post_process.rs:274` each hold a render-world
`Local<HashMap<TextureViewId, BindGroup>>` with no eviction and no `.clear()`
anywhere. Their in-code comments explicitly bet on "a kiosk app that never
resizes." Workstream 3 deliberately breaks that bet. Each stranded entry pins a
full-screen `Rgba16Float` texture for the life of the process.

Slower than 2.1 and not the cause of these crashes, but real.

### 2.3 Window-resize invalidation (one cause, three symptoms)

Nothing in Line, Dots, or Flame reacts to `WindowResized`. Each reads
`window.width()` / `window.height()` exactly once, at spawn:

- `crates/wc-sketches/src/line/systems/spawn.rs:158`
- `crates/wc-sketches/src/dots/systems/spawn.rs:157-158`
- `crates/wc-sketches/src/flame/systems/spawn.rs:111-112`

Only `hand_mesh/mod.rs:218` and `cymatics/render.rs:104` subscribe to the event.

This explains three separate reports:

1. **"F11 gives framed fullscreen."** The window *does* go fullscreen; the
   sketch keeps drawing its particle field into the old extent. Navigating to
   the menu and switching scenes respawns the sketch, which re-reads the window,
   and it then fills the screen. That is exactly the tester's repro.
2. **UI panels load at the wrong size and fall off the right edge, then correct
   themselves.** Windows settles the scale factor a frame or two after window
   creation; egui lays out against the stale size.
3. **Particle counts drift in the log.** `count=12800` is `10 × 1280`;
   `count=15360` is `10 × 1536`. The window resized, and a later respawn picked
   up the new size.

### 2.4 ONNX DirectML execution provider fails fatally

`crates/wc-core/src/input/providers/mediapipe/inference_ort.rs:98` propagates a
DirectML failure from `commit_from_memory` as a fatal `InferenceError::Load`.
The guard at `inference_ort.rs:208-217` catches errors only from `register()`,
not from commit. DirectML registers successfully (the Vega 10 is a valid DX12
device), then throws `80004005` inside `DmlGraphFusionHelper.cpp` during graph
fusion.

The doc comments at `inference_ort.rs:6-9` and `:62-63` claim ONNX Runtime "falls
back to CPU for any op the EP cannot place, so load never fails closed." This
conflates per-op placement fallback with an EP crashing at commit time. It is the
assumption that produced the bug.

Consequence: with no Leap device attached, Windows has **no hand tracking at
all** — the provider chain falls through to `MockProvider`.

### 2.5 Leading hypothesis for the DirectML failure

Static analysis of the vendored models (`assets/models/hand/`):

| Model | PRelu nodes | Slope shape | Rank | Pad / Resize / Concat |
| --- | --- | --- | --- | --- |
| `palm_detection.onnx` (shipped) | 26 | `(C,1,1)` | 3 | 3 / 2 / 2 |
| `palm_detection_original.onnx` | 26 | `(1,C,1,1)` | 4 | 3 / 2 / 2 |
| `hand_landmark.onnx` | 0 | — | — | 0 / 0 / 0 |

The two palm variants are otherwise identical (`Conv=53` in both). The sole
delta is the slope rank, introduced in commit `d2369f4f` **for CoreML**, whose
NeuralNetwork EP requires `[C,1,1]` or a scalar and rejects `[1,C,1,1]`
(`docs/runbooks/onnx-coreml-model-surgery.md:123-125`). That surgery predates
any Windows GPU-inference build.

DirectML's operators are rank-specific, and `DmlGraphFusionHelper` is the
partitioner that must place `PRelu` between 53 convolutions.
`mod.rs:276-277` loads palm **before** landmark, and the tester's log shows
exactly one initialization exception before the provider bails — so the model
that failed is the surgically altered one, and the model with no `PRelu` was
never reached.

**Hypothesis:** the CoreML fix is what broke DirectML.

This is a hypothesis, not a result. It has not been demonstrated that DirectML
rejects rank-3 slopes. What has been established is that the sole difference
between the failing model and its unmodified upstream original is a rank change
made for an unrelated platform, and that the clean model carries none of the
suspect ops. Workstream 7b tests it.

If confirmed, the fix requires no model surgery at all:
`palm_detection_original.onnx` is already committed, `load_model` takes a
filename, and Windows can `cfg`-select rank-4 while macOS keeps rank-3 —
restoring **full DirectML acceleration for both models**.

### 2.6 Windows platform gaps

- **No startup fullscreen.** `crates/waveconductor/src/main.rs:52-57` creates a
  1280×720 windowed window. The only path to fullscreen is the F11 keybind at
  `lifecycle/action_map.rs:104`, undocumented for a field tester. Nothing
  re-asserts fullscreen when a monitor re-enumerates.
- **Wrong audio device.** `audio/engine.rs:139-146` takes
  `cpal::default_host().default_output_device()`. No enumeration, no picker.
  Worse, on stream error `audio/state.rs:185` logs "Restart the app to recover
  audio" and means it: there is no reopen path. One HDMI blip silences an
  unattended installation for the night.
- **Thermal blindness.** `lifecycle/thermal/platform/windows.rs:71-77` degrades
  to `Cool`/`Schedule` when no WDDM adapter reports a temperature. The tier pins
  to `Cool` forever, and `Cool` selects the *no-reduction* branch of the
  screensaver present-rate throttle — the only thermal lever in the app. The
  failure is logged at INFO, so it goes unnoticed.

### 2.7 Cymatics cold start

`cymatics/mod.rs:389` (`init_cymatics_state`) runs on
`OnEnter(AppState::Cymatics)`, and the ping-pong textures are allocated fresh
and zeroed **on every entry**. The blank field therefore appears each time a
visitor navigates to Cymatics, not once at boot. The tester's log shows
`navigate target=Cymatics` at 23:22:42, 23:22:58, 23:23:23 and 23:23:31 — four
cold starts in under a minute. Booting into attract mode does not address this.

### 2.8 AGENTS.md is inaccurate

AGENTS.md claims "every per-sketch resource is owned by an entity tagged with the
sketch's marker component, despawned on `OnExit` to release VRAM." That is not
what the code does. The invariant is upheld by a web of explicit render-world
removal systems plus bounded, evicting caches — and §2.2 shows two caches that
break it.

## 3. Non-goals

- **Migrating to Windows ML.** It ships CPU and legacy DirectML in-box; hardware
  providers come from the `ExecutionProviderCatalog` and depend on device and
  driver. Nothing in that catalog targets a 2018-era GCN5 iGPU, so WinML routes
  to DirectML and lands on the identical stack trace. It also imposes a Windows
  11 24H2 floor (cutting off the only field-test box), replaces a self-contained
  MSI with a runtime that downloads execution providers on first use (a network
  dependency for an offline kiosk), and requires WinRT interop while `ort` is
  retained for macOS anyway. Revisit if the deployment box becomes a Ryzen AI
  machine and NPU offload is wanted. The commit-time fallback in 7a is required
  under any runtime, so it is not wasted work.
- **VRAM budget telemetry** (`IDXGIAdapter3::QueryVideoMemoryInfo`). Deferred.
- **`ThermalSource::GpuTimeProxy`.** The enum variant stays declared and unbuilt.
- **Threshold tuning** for the thermal tiers. The current values are documented
  placeholders pending soak data from real hardware; they cannot be tuned from a
  dev machine. Alpha.5 ships the sensor and the logging that makes tuning
  possible later.
- **Per-backend inference latency benchmarking as a release gate.** The probe
  tool reports latency, but the decision about whether DirectML is even the right
  default on a shared-die APU is a follow-up.
- **Attract-mode idle timeout slider and boot-into-attract-mode.** Owned by the
  in-flight `configurable-attract-mode-timeout` worktree. This spec never touches
  them.

## 4. Design

### Workstream 1 — Eliminate the composite bind-group leak

**Files:** `crates/wc-core/src/ui/blur/callback.rs`, `.../blur/mod.rs`,
`crates/wc-core/src/ui/{buttons.rs,frame.rs}`, `crates/wc-core/tests/ui_blur.rs`

Remove `Box::leak`. Move the per-widget uniform write and bind-group creation out
of `render()` and into the `update()` hook (`callback.rs:229-236`), which is
currently empty and already receives `&mut World`.

**Verified against `bevy_egui` 0.40 source** (`src/render/render_pass.rs`):

- `prepare_egui_pass` (line 20) is a render-graph node that drains
  `postponed_updates` and calls `update(...)` on each (line 38). `egui_pass`
  later calls `render(...)` per draw command (line 230). **All `update()` calls
  precede all `render()` calls.** Write-in-update, read-in-render is sound.
- `render_entity` is **not** per-callback. `EguiRenderData` carries a single
  `render_entity: RenderEntity` field (`src/render/systems.rs:303`) beside
  `postponed_updates: Vec<(egui::Rect, PaintCallbackDraw)>` (`:311`), and
  `render()` receives `RenderEntity::from(ui_view_entity)` (`render_pass.rs:234`).
  Every callback in a context shares it. Keying per-widget state on it would make
  all frosted panels render with the last panel's rect. (The upstream
  `paint_callback.rs` example is safe only because it stores a pipeline id,
  identical for every callback.)
- `render()` is guarded by `if viewport.width_px > 0 && viewport.height_px > 0`
  (`render_pass.rs:218`) while `update()` is not. A zero-sized widget receives an
  `update()` with **no matching `render()`**. Any index- or cursor-based pairing
  between the two would silently desynchronize.

Therefore: key slots by a stable `egui::Id`.

Add `id: egui::Id` to `BackdropBlurPaintCallback` (currently only
`corner_radius` and `rect`, `callback.rs:216-223`). Both construction sites —
`buttons.rs:334` and `frame.rs:81` — sit inside egui UI code where `ui.id()` is
available.

Introduce a render-world resource:

```rust
struct CompositeSlot {
    buffer: Buffer,
    bind_group: BindGroup,
    blur_view: TextureViewId,
    last_seen: u64,
}
struct CompositeSlots(HashMap<egui::Id, CompositeSlot>);
```

- `update()` gets or creates the slot for `self.id`, calls `queue.write_buffer`
  on the existing buffer (no allocation), and rebuilds the bind group **only**
  when the blur texture's `TextureViewId` changes (on resize, not per frame).
  Stamps `last_seen`.
- `render()` reads `&'pass CompositeSlots` from `world: &'pass World`, yielding a
  `&'pass BindGroup` for `set_bind_group`. The `'pass` lifetime falls out for
  free — precisely what `Box::leak` was faking.
- Slots unseen for 600 frames (~10 s at 60 fps) are evicted, bounding the map if
  ids churn. The eviction sweep runs in `update()`, not per-slot.

Bounded by widget count. Zero steady-state allocation. No positional coupling.

Also add `Box::leak` to `disallowed-methods` in `clippy.toml`.

**Test:** the slot bookkeeping is factored into a GPU-free generic
(`SlotBook<T>`) whose eviction and bounded-growth properties are unit-tested
with `T = ()`. The regression test that matters — *a widget painted every frame
for 5000 frames occupies exactly one slot* — then runs on every CI push with no
GPU.

`crates/wc-core/tests/ui_blur.rs` is **not** the right home for this. Every test
in that file is `#[ignore]`d because `DefaultPlugins` pulls in winit, which
requires the macOS main thread while cargo's test runner uses worker threads
(`ui_blur.rs:7-18`). `cargo nextest` skips ignored tests, so an assertion added
there would never execute in CI.

### Workstream 2 — Bound the post-process bind-group caches

**Files:** `crates/wc-sketches/src/line/post_process.rs`,
`crates/wc-sketches/src/dots/post_process.rs`

Replace each `Local<HashMap<TextureViewId, BindGroup>>` with the shape upstream
already uses in `bevy_core_pipeline::fullscreen_material::FullscreenMaterialBindGroup`
(`fullscreen_material.rs:244-277`): **two slots, one per ping-pong view, each
validated against the `TextureViewId` of the very texture view it binds.**

An earlier draft of this spec proposed keying eviction on
`camera.physical_target_size`. That is a proxy, and review showed it is a subset
of the real condition: Bevy reallocates a `ViewTarget` whenever any of
`(camera.target, texture_usage, main_texture_format, Msaa)` changes. It happens
to be correct today only because nothing mutates HDR, MSAA, or the render target
on this camera after `spawn_camera` (`crates/waveconductor/src/main.rs:250-282`)
— an unwritten invariant that the deferred per-tier quality scaling in §10.4 of
the thermal design would break.

Comparing the bound view's own id catches every reallocation cause without
knowing which occurred, is bounded at two entries by construction rather than by
an eviction policy, and cannot rot when Bevy adds a dimension to that key.

Update the stale "kiosk app that never resizes" comments; they are now false.

**Upstream observation, recorded for a possible Bevy issue.** Bevy's other
pattern, `TonemappingBindGroupCache` (`tonemapping/node.rs:21`), is a *single*
slot keyed on `source.id()`. But `ViewTarget::post_process_write()` does
`fetch_xor(1)` on an `Arc<AtomicUsize>` that Bevy deliberately reuses across
frames (`view/mod.rs:1307`: *"re-use the same atomics frame to frame ... to
ensure post process writes persist through msaa writeback"*), and never resets.
So the source view a node sees alternates every frame whenever the per-frame
count of `post_process_write()` calls is odd — which means the tonemapping cache
**never hits** under those configurations, and hits every frame under even ones.
Toggling one post-process effect silently flips it. This is not a correctness
bug (the miss path is correct) and the wasted `create_bind_group` is a few
microseconds per camera per frame, so it is not a performance argument either.
It is a correctness-of-intent defect: the cache exists precisely to avoid that
allocation and, in common configurations, does nothing. Worth an upstream issue
proposing tonemapping adopt the two-slot `fullscreen_material` shape. Not worth
a performance-framed PR, and not on this release's critical path.

### Workstream 3 — Window-resize invalidation

**New:** `crates/wc-core/src/lifecycle/window_resize.rs` (a named module under
`wc-core`, not a `utils/` dumping ground).

Consumes `WindowResized` and `WindowScaleFactorChanged`, debounces 250 ms after
the last event, and emits a `WindowResizeSettled` message. Debouncing prevents respawn thrash while
a window edge is dragged; in kiosk use, resize only occurs at F11 and at startup
scale-factor settle.

Line, Dots, and Flame each gain a listener that re-runs their existing spawn
path. Each is gated on its own sketch being active so nothing runs in
`SketchActivity::Idle`, per AGENTS.md. Cymatics already resizes its quad but
derives its sim grid from window aspect at spawn, so it re-inits its grid on the
same signal.

**Spike required.** The egui panel misplacement (§2.3 symptom 2) has a known
trigger but an unlocated stale rect. Scope a short investigation into
`crates/wc-core/src/settings/panel_user/dock.rs` before committing to a fix.
This is deliberately left unspecified rather than guessed at.

### Workstream 4 — Fullscreen and display settings

**New:** `crates/wc-core/src/settings/panel_user/display.rs` (one concern per
file). Its rows are only: Start fullscreen, Hide cursor, Monitor, Audio output.
"Boot into attract mode" and "Attract after N s idle" belong to the
`configurable-attract-mode-timeout` worktree and are not touched here.

New settings fields:

- `start_fullscreen: bool` — default `true` in release, `false` under
  `debug_assertions` so `cargo rund` stays sane.
- `hide_cursor: bool`
- `monitor: Option<String>` — persisted by name.

A startup system applies the mode. The existing F11 handler
(`lifecycle/nav.rs:63-71`) writes `start_fullscreen` back on toggle. Monitors are
enumerated from Bevy's `Monitors`; an absent saved monitor falls back to
`Current`. Fullscreen and monitor selection are **re-asserted on `MonitorAdded`
/ `MonitorRemoved`**, which is what lets the app survive a TV that sleeps and
re-enumerates mid-soak.

### Workstream 5 — Audio output device selection and recovery

**Files:** `crates/wc-core/src/audio/{engine.rs,state.rs,mod.rs}`

Enumerate `host.output_devices()`, persist the choice by name, resolve at startup
with a fall-back to the system default.

The recovery half matters more. A supervisor replaces the terminal
`AudioStatus::Errored`:

- The existing cpal error-callback flag (`engine.rs:223-229`) triggers a stream
  rebuild with backoff (1 s, 2 s, 4 s, capped at 30 s).
- A background poll (~2 s) watches device topology so the stream migrates back
  when the saved endpoint reappears.
- Rebuild re-resolves the device, recreates the stream, and restores play/pause
  from `AppState`.
- `AudioStatus` gains a `Reconnecting` variant. The "Restart the app to recover
  audio" string at `state.rs:185` is removed.

Enumeration and rebuild run off both the audio callback and the render thread;
WASAPI enumeration can block. The audio thread's real-time constraints are
unchanged: no locks, no allocation after init.

### Workstream 6 — Thermal sensor chain

**Files:** `crates/wc-core/src/lifecycle/thermal/platform/windows.rs`,
`crates/wc-core/Cargo.toml`

Chain: **WDDM D3DKMT** (existing, direct GPU-die temp) → **WMI ACPI thermal
zone** (new) → **Schedule** (existing fallback). `ThermalTier` and its hysteresis
are unchanged.

Rationale for WMI as the second rung: WaveConductor is GPU-bound and its only
lever (the screensaver present-rate throttle) reduces GPU work, so GPU
temperature is the signal in the abstract. But both deployment candidates (Vega
10, Radeon 780M) are APU iGPUs sharing one die and one thermal budget with the
CPU cores, so the package temperature that ACPI zones expose is a faithful proxy
— and it is available precisely on the hardware where the WDDM query returns 0.

Implementation notes:

- Read `Win32_PerfFormattedData_Counters_ThermalZoneInformation` from
  `root\CIMV2`, which generally reads **without elevation**, in preference to
  `MSAcpi_ThermalZoneTemperature` in `root\WMI`, which often requires admin.
  Verify on the target box rather than assuming.
- New `windows` crate **features** only (`Win32_System_Com`, `Win32_System_Wmi`,
  `Win32_System_Variant`). No new dependency.
- Filter zones to a plausible 1–150 °C after Kelvin conversion; the hottest
  plausible zone wins (conservative).
- COM must be initialized per-thread. The query lives on the existing thermal
  sampler thread, never the render thread.
- Escalate the "no sensor" line from INFO to **WARN**. Silent thermal blindness
  is how this went unnoticed.
- Log provenance (`source=wmi-zone zone="..." temp=61.3C`) and sample
  periodically, so the next soak log carries the data needed to tune thresholds.

Caveat to record: ACPI zone semantics are OEM-defined. Some boxes report a
chipset or chassis-skin temperature rather than the die, so thresholds tuned on
one machine do not transfer. Skin and package temps also lag die temps by tens of
seconds, which is acceptable here because the lever is an attract-mode present
rate, not frame-level control.

**Test:** put the zone filtering and hottest-zone selection behind a trait so the
selection logic is unit-testable without a WMI call.

### Workstream 7a — ONNX execution-provider resilience (ships regardless)

**File:** `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs`

`OrtInference::load` gains a commit-level retry. Attempt the DirectML-configured
builder and `commit_from_memory`; on error, log a warning naming the failing
model, construct a **fresh** CPU-only `SessionBuilder` (skipping both
`configure_accelerator_session` and `register_accelerator`), commit that, and
return `BACKEND_CPU`. The builder is consumed by the failed commit, hence the
rebuild; the cost is paid only on the error path. Apply the same shape on macOS,
since CoreML commit can also fail (see the cache-staleness incident in
`docs/runbooks/onnx-coreml-model-surgery.md`).

**DirectML is still used wherever it commits.** This changes only failure
behavior.

Per-model fallback is already free. `combined_backend(palm, landmark)`
(`mediapipe/mod.rs:512-523`) takes two backend labels and `BACKEND_DIRECTML_CPU`
("ort/DirectML+CPU") already exists. `load_model` is called once per model, so if
only `palm_detection.onnx` trips DML fusion, `hand_landmark.onnx` keeps running
on DirectML.

Also:

- Correct the doc comments at `inference_ort.rs:6-9` and `:62-63`.
- Add a `hand_tracking_backend` setting: `Auto` / `ForceGpu` / `ForceCpu`, so the
  field tester can A/B without a new build.
- Log the effective per-model backend at startup and, on EP failure, the exact
  failing node, so the existing "upload the log" workflow carries the diagnostic
  without the tester running anything new.

### Workstream 7b — DirectML remediation ladder (separate branch)

Goal: keep the iGPU accelerated rather than settling for CPU. CPU is the floor,
not the plan.

**Tool first.** `cargo xtask probe-ep --model <path> --ep directml|coreml|cpu
[--graph-opt level1|level3|disable] --json`. Builds a session, commits, and
reports success or the exact error, node-placement counts per EP, partition
count, and mean inference latency. `xtask` depends on neither Bevy nor `wc-core`,
so this builds in minutes on a fresh Windows clone. It adds `ort` to `xtask`
reusing the workspace pin (`=2.0.0-rc.12`), not a new dependency.

Ladder, cheapest first, each rung falsifiable by the probe:

0. **The PRelu-rank experiment (§2.5).** Probe all three models on DirectML.
   Because the hypothesized trigger is device-independent, an RDNA2 discrete GPU
   is a valid test bed:

   | Result | Conclusion |
   | --- | --- |
   | Surgered palm fails, original palm passes | PRelu rank confirmed. `cfg`-select rank-4 on Windows, rank-3 on macOS. Both models accelerate. |
   | Both palm variants fail | It is Pad/Resize/Concat. Model-level, fixable without the field tester. |
   | Both pass | GCN5- or driver-specific. Cheap causes excluded; escalate to the field tester's box. |

1. **Ship a newer `DirectML.dll`.** The staged DLL
   (`xtask/src/bundle/windows.rs:244`) is whatever pyke's `ort` rc.12 built
   against. DirectML is independently redistributable and fusion fixes for older
   GCN hardware land in it.
2. **Lower the graph optimization level** (`Level1`, then `Disable`). ONNX
   Runtime issue #12538 suggests this may not rescue DML, but it is one flag and
   instantly falsifiable.
3. **`optimization.disable_specified_optimizers`** against named transformers.
4. **A different ONNX Runtime build.** `ort` statically links pyke's ORT; given
   the documented 16.3 → 17.0 DML initialization regression (issue #21205),
   testing Microsoft's official `onnxruntime.dll` via `load-dynamic` establishes
   whether the failure is build-specific.
5. **Model surgery, DirectML edition.** Only if 0–4 fail. The CoreML runbook
   already names the suspects: channel-dim `Pad`, `half_pixel` `Resize`, 3-D
   `Concat`, `PRelu` slope shapes.

Keep the probe's latency column in view throughout. On a shared-die APU,
DirectML inference contends with the renderer for the same shader cores and the
same memory pool, and the renderer is already the bottleneck. CPU inference on
two ~4 MB models may be faster end-to-end than DirectML on a Vega 10. Winning
this ladder and then measuring a regression is a real possible outcome; the
measurement costs one JSON field.

### Workstream 8 — Cymatics warm start

**File:** `crates/wc-sketches/src/cymatics/`

Seed texture A with the resting two-blob field on `OnEnter(AppState::Cymatics)`
rather than starting from a zeroed texture, so navigating to Cymatics never shows
a blank frame. Self-contained; no interaction with any other workstream.

### Workstream 9 — Correct AGENTS.md

Replace the inaccurate GPU-resource claim (§2.8) with the actual invariant:
entity-owned resources are despawned on `OnExit`; render-world `Resource`s and
`Local` caches require explicit removal systems or bounded, evicting caches.
Record `Box::leak` in a render callback as a hot-path allocation, and note the
new `clippy.toml` lint that enforces it.

## 5. Branch and sequencing plan

```
v5-alpha
  └── configurable-attract-mode-timeout        (in flight, merges first)
        └── windows-remediation                (workstreams 1-6, 7a, 8, 9 → alpha.5)
              └── windows-directml-prelu-rank  (workstream 7b: probe tool + experiment + candidate fix)
```

`windows-remediation` branches from `v5-alpha` **after**
`configurable-attract-mode-timeout` merges, so the Display settings section lands
on a settled `settings/panel_user/`.

The **entire DirectML investigation** — probe tool, model override, graph-opt
flags, and any candidate fix — lives on `windows-directml-prelu-rank`, pushed to
the remote as a self-contained thing. Rationale: the model swap could regress
Windows further, and rank-4 slopes would definitely regress CoreML if they leaked
to macOS. Nothing speculative merges until the probe returns data. The Windows
machine checks out exactly one branch, builds `xtask` only, and runs the probe.

Workstream 7a stays on `windows-remediation` because it ships regardless of what
the experiment finds.

**Work location.** Everything except the probe run happens on the macOS dev
machine, including the `probe-ep` tool itself (exercised against CoreML and CPU
locally so it is known-good before it sees Windows). Rungs 1–3 are wired as
*flags* rather than rebuilds, so a single Windows build tests every rung. The
field tester validates the outcome; he does not iterate.

## 6. Verification

- Unit tests per module, colocated as `#[cfg(test)] mod tests`.
- Workstream 1's leak regression is a GPU-free unit test over `SlotBook<T>`, not
  an integration test in `crates/wc-core/tests/ui_blur.rs` — everything in that
  file is `#[ignore]`d for winit main-thread reasons and never runs in CI.
- Thermal zone selection is unit-tested behind a trait (workstream 6).
- Full CI gate per AGENTS.md: `cargo fmt --check`, `cargo clippy --all-targets
  --all-features --workspace -- -D warnings`, `cargo nextest run --workspace
  --all-features`, `cargo test --doc --workspace`, `cargo doc --no-deps`,
  `cargo deny check`, `cargo xtask check-secrets`.
- `cargo xtask capture` on the affected sketches after workstream 3, to confirm
  the debounced respawn does not destabilize the deterministic capture harness.

**The real gates**, in order:

1. Alpha.5 survives well past the current 5–13 minute window on the field
   tester's box, with a clean log.
2. `probe-ep` on Windows returns a verdict on the PRelu-rank hypothesis.
3. An 8-hour soak on the deployment mini PC, watching RSS, GPU memory, and FPS.

## 7. Risks

- **Workstream 1's bevy_egui contract.** Verified against 0.40 source, but it is
  an internal invocation-order property of a third-party crate, not a documented
  API guarantee. A `bevy_egui` bump could change it. The `egui::Id` keying makes
  the code robust to the ordering *and* the update/render count mismatch, so a
  future bump degrades to a correctness bug in slot lifetime, not silent
  cross-panel corruption.
- **Workstream 3's egui panel bug is unscoped.** Deliberately marked as a spike
  rather than guessed at. It may not resolve with the sketch respawn fix.
- **Workstream 5's WASAPI enumeration** can block; it must never run on the audio
  or render threads. The device-topology poll is the riskiest new background
  work in this spec.
- **Workstream 6's WMI/COM initialization** on the thermal sampler thread. WMI
  queries are slow (tens of ms); the sampler's cadence must accommodate it.
- **Thermal thresholds remain untuned** after alpha.5 by design. A box that
  reports temperature will now classify tiers against placeholder thresholds,
  which is a behavior change from "always Cool". This is the intended direction
  (the safe failure is to throttle a cold machine, not to bake a hot one), but it
  should be watched in the first soak.
- **Rebasing on `configurable-attract-mode-timeout`.**

## 8. Follow-ups (explicitly deferred)

- Per-backend inference latency measurement to decide whether DirectML is the
  right default on a shared-die APU.
- VRAM budget telemetry via `IDXGIAdapter3::QueryVideoMemoryInfo`.
- `ThermalSource::GpuTimeProxy`.
- Thermal threshold tuning against real soak data.
- An agent-operable `cargo xtask soak-test`.
- Instruction-screen overlay (never built; tracked separately).
- **Upstream Bevy issue:** `TonemappingBindGroupCache` misses every frame when
  an odd number of `post_process_write()` calls occur per frame, because the
  ping-pong parity atomic persists across frames and is never reset. Propose it
  adopt the two-slot `fullscreen_material` shape. File as an issue first, not a
  PR: it is a correctness-of-intent defect, not a measurable performance win, and
  the shared parity atomic is load-bearing for MSAA writeback. See Workstream 2.
