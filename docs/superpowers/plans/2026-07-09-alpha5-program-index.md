# Alpha.5 Program Index — Plans 02 through 08

**Date:** 2026-07-09
**Spec:** `docs/superpowers/specs/2026-07-08-windows-remediation-design.md`
**Plan 01 (complete):** `docs/superpowers/plans/2026-07-08-alpha5-01-gpu-memory-leak.md` — merged to `v5-alpha` at `54df33b0`

This is the connective tissue between the spec and the individual plans. It carries the
context that would otherwise be lost between sessions: what each plan is for, what has
already been decided, what is still genuinely unknown, which anchors in the code they
touch, and which plans can run at the same time.

Every code anchor below was re-verified against `v5-alpha` at `54df33b0`. If a line number
is off by a few, search for the quoted symbol rather than trusting the number.

---

## Part 1 — Shared facts, learned the hard way

These bit us during Plan 01. Every plan inherits them. An implementer that does not know
them will waste a review cycle rediscovering them.

### The settings system is decentralised

There is **no** monolithic `settings.rs`. Each subsystem owns a settings struct in its own
module, derives `SketchSettings`, and registers itself:

```rust
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ScreensaverSettings { /* … */ }   // crates/wc-core/src/lifecycle/screensaver/settings.rs:39-42
```

`SketchSettings` (`crates/wc-core/src/settings/trait_def.rs:24`) requires a `STORAGE_KEY` and
`fn settings_def() -> Vec<SettingDef>` (`:44`). Registration is one call to
`App::register_sketch_settings::<S>()` (`crates/wc-core/src/settings/registry.rs:169-175`), and
the user panel renders the section from `settings_def()`.

**Consequence for parallelism:** three different plans can each add a settings section without
touching a shared file. This is why Plans 03, 04 and 06 are *not* serialised on each other.
Existing examples to copy: `lifecycle/screensaver/settings.rs`, `settings/hand_tracking.rs`,
`wc-sketches/src/line/settings.rs`.

### The per-task clippy gate must use `--all-targets`

`cargo clippy -p <crate> --lib` skips the test target. CI runs `--all-targets`. In Plan 01
that gap hid `clippy::range_plus_one` and `clippy::used_underscore_binding` in our own test
code until the whole-workspace gate ran. Always:

```bash
cargo clippy -p <crate> --all-targets --all-features -- -D warnings
```

### Clippy is `-D warnings` over `pedantic`, **including inside `#[cfg(test)]`**

`Cargo.toml:206-211` sets `pedantic = warn` plus `unwrap_used`, `expect_used`, `panic`, and
`as_conversions` at `warn`. CI escalates all warnings to errors and passes `--all-targets`, so test code
is held to the same bar as production code. Four of Plan 01's plan defects were this:

- `.expect(…)` / `.unwrap()` in a `#[cfg(test)] mod tests` block is denied **unless the block opts out**.
  The house convention (verified 2026-07-09 in `settings/hand_tracking.rs:168`,
  `mediapipe/inference_ort.rs:346`, and `mediapipe/mod.rs:686`) is to put
  `#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]` directly on the
  `mod tests` declaration. Reuse it in an existing block; **add it when you create a new one** — that
  omission is what bit Plan 01. Bare `panic!` is still denied (`clippy::panic`).
- `assert_eq!(x.is_some(), true)` → `clippy::bool_assert_comparison`. Use `assert!(x.is_some())`.
- `0..(N + 1)` → `clippy::range_plus_one`. Use `0..=N`.
- `u._pad` → `clippy::used_underscore_binding`.

A plan that puts any of these in its own example code hands the implementer a build failure.

### Test-only helpers need `#[cfg(test)]`, and a type with no non-test caller is dead code

An accessor used only from `mod tests` trips `dead_code` on the lib target. Worse: a whole new type with
no production caller yet (because the caller lands in a later task) fails `-D warnings` on the lib target
for every task in between. Plan 01 solved this with a transient `#![allow(dead_code)]` carrying an
explicit deletion step in the task that introduced the first real caller. If a plan introduces a type
before its consumer, it **must** say so and schedule the removal.

### The doc gate has no `--all-features`, and denies public→private intra-doc links

CI runs exactly `cargo doc --no-deps --workspace --document-private-items` with
`RUSTDOCFLAGS="-D warnings"` (`.github/workflows/ci.yml:208`). Two traps:

- **Do not add `--all-features`** when reproducing it. Feature-gated modules (`leap_native`,
  `template_picker`) surface unrelated errors and send you chasing ghosts.
- A **public** item's rustdoc linking to a `pub(crate)` item trips
  `rustdoc::private_intra_doc_links`, which is denied. Demote to a plain code span. This broke
  Plan 01 twice. Public trait-impl methods count as public.

### Commit messages: `-F <file>`, never `-m`

Backticks inside a `-m` string are command-substituted by zsh and silently eat words. Write the
message to a file and `git commit -F`.

### Subagents must never `git add -A`

Stage named paths only, then `git show --stat HEAD` to confirm. During Plan 01 the working tree
carried an unrelated in-progress CI refactor for hours.

### There are no GPU tests in CI

Everything in `crates/wc-core/tests/ui_blur.rs` is `#[ignore]`d, because `DefaultPlugins` pulls
in winit, which demands the macOS main thread while cargo's runner uses worker threads
(`ui_blur.rs:7-18`). `cargo nextest` skips ignored tests. **Any assertion placed there never
runs.** Design regression tests as GPU-free unit tests over an extracted pure structure, the way
`SlotBook<T>` was extracted in Plan 01.

Corollary: `cargo xtask capture` returns all-black `[0,0,0]` frames when the app window is not
foregrounded, so an agent cannot use it to verify rendering. **A human must run `cargo rund`.**
Plan 01's Critical regression — the blur silently never drawing — was invisible to every
automated gate and was only caught by a whole-branch review plus Madison's eyes.

### `bevy_egui` 0.40: `update()`'s `PaintCallbackInfo` is partly garbage

`EguiRenderTargetData::target_size` is declared (`render/systems.rs:313`), zero-initialised
(`:331`), read to build the `info` passed to every paint callback's `update()` hook
(`render/render_pass.rs:33`) — and **never assigned anywhere in the crate**. So
`info.screen_size_px == [0, 0]` in `update()`. `render()`'s copy is built from the camera
viewport and is correct. `info.pixels_per_point` *is* assigned (`render/systems.rs:378`) and is
safe. Read window size from `ExtractedWindows` instead. Recorded in the spec as an upstream bug
worth reporting.

### Bevy render-world facts worth not re-deriving

- `TextureViewId` is a process-global monotonic counter (`define_atomic_id!`), never recycled. An
  id comparison is therefore a sound validity check for any cached bind group.
- Bevy reallocates a `ViewTarget` on any change to
  `(camera.target, texture_usage, main_texture_format, Msaa)` (`bevy_render/src/view/mod.rs:1253`),
  **not** only on resize. Never key a cache on window size as a proxy.
- `ViewTarget::post_process_write()` does `fetch_xor(1)` on an `Arc<AtomicUsize>` that Bevy
  deliberately reuses across frames and never resets (`view/mod.rs:1307`). So which of the two
  ping-pong views you see alternates per frame iff the per-frame count of `post_process_write()`
  calls is odd.
- The canonical cache shape is
  `bevy_core_pipeline::fullscreen_material::FullscreenMaterialBindGroup` (`fullscreen_material.rs:244-277`):
  two slots, one per ping-pong view, each validated against the `TextureViewId` it binds.

### AGENTS.md now states three GPU-release mechanisms

Entity-owned (dropped by `OnExit` despawn); render-world `Resource`s (need explicit removal
systems); render-world `Local` caches (must be bounded by construction). `Box::leak` is banned in
`clippy.toml`. Read that bullet before touching render-world state.

---

## Part 2 — The plans

### Plan 02 — Window-resize invalidation

**Goal.** One root cause, three of the field tester's reports.

**Why.** Nothing in Line, Dots or Flame reacts to `WindowResized`. Each reads the window size
exactly once, at spawn. Only `hand_mesh/mod.rs` and `cymatics/render.rs` subscribe to the event
(verified: those are the *only* two files in `crates/` mentioning `WindowResized`).

**Evidence from the field.** The tester wrote: *"F11 to framed fullscreen, esc goes to real
fullscreen menu and it stays fullscreen from then on"* and *"it gets fixed when I hit escape to go
to main menu then z/x to switch scenes!"* F11 **does** fullscreen the window; the sketch keeps
drawing its particle field into the old extent. Navigating away and back respawns the sketch,
which re-reads the window. His log corroborates: `count=12800` is `10 × 1280`, and `count=15360` is
`10 × 1536`.

**Anchors.**
- `crates/wc-sketches/src/line/systems/spawn.rs` — count derived from `particle_density × window.width()` (doc at `:117`, code near `:158`)
- `crates/wc-sketches/src/dots/systems/spawn.rs:157-158` — grid `cols × rows` from window size
- `crates/wc-sketches/src/flame/systems/spawn.rs:111-112`
- `crates/wc-sketches/src/cymatics/render.rs` — already resizes its quad, but the **sim grid** is derived from window aspect at spawn
- `crates/wc-core/src/settings/panel_user/dock.rs` (230 lines) — the egui panel spike, below

**Decisions locked.** Debounced respawn, 250 ms after the last event. New module
`crates/wc-core/src/lifecycle/window_resize.rs` consuming `WindowResized` **and**
`WindowScaleFactorChanged`, emitting a `WindowResizeSettled` message. Each sketch listens and
re-runs its existing spawn path, gated on that sketch being active so nothing runs in
`SketchActivity::Idle`. Rejected: rescale-in-place (Dots' grid count genuinely changes, so it must
reallocate regardless).

**Spike resolved 2026-07-09. There is no stale rect in our code.** `dock_rect` is a pure function
(`dock.rs:112`) recomputed every frame from the live window (`panel_user/mod.rs:162-169`), and the
`egui::Area` is re-pinned every frame with `fixed_pos` + `set_min_size`/`set_max_size`
(`mod.rs:200-207`). Nothing is cached anywhere on our side.

The defect is a **one-frame lag in `bevy_egui`**. `update_ui_screen_rect` (`bevy_egui/src/lib.rs:1868`,
scheduled in `PreUpdate`) computes egui's `screen_rect` as
`camera.physical_viewport_rect() / egui_output.pixels_per_point`. But `egui_output.pixels_per_point` is
written *after* the egui pass, from the previous frame's `FullOutput` (`output.rs:56`), and its default
is `1.0` (`lib.rs:633`). The current scale factor does reach egui as
`egui_input.viewports[ROOT].native_pixels_per_point = camera.target_scaling_factor()`
(`input.rs:1247`), but `screen_rect` is derived from the **stale** output value, not that one.

We then compound it: `dock_rect` is fed `Window::width()`, which is **Bevy logical pixels**, while
`egui::Area::fixed_pos` consumes **egui points**. Those agree in steady state (`zoom_factor == 1.0`),
and disagree for exactly the frames where `pixels_per_point` is stale. At 125% DPI, frame 1 has egui
believing the screen is 2400 points wide while we place the dock at `x = 1264` — misplaced and drawn
at 1/1.25 scale, then snapping into place. That is the reported symptom precisely.

This reproduces on macOS too (`pixels_per_point == 2.0`); we never see it because the panel defaults to
closed (`SettingsPanelVisible` default `false`) and the transient ends before anyone opens it. **The
tester sees it because F11 changes the scale factor**, re-triggering the lag with the panel open. That
is why he reported the panel bug and the fullscreen bug together — same trigger.

**Fix (in scope for this plan, small).** Derive the dock geometry from `ctx.screen_rect()` instead of
querying Bevy's `Window`. Both sides then speak points, nothing is mixed, and during the stale frame the
dock stays anchored inside whatever egui believes the screen to be rather than overflowing it. This
deletes the `Window` query at `mod.rs:162-168`. `dock_rect` stays pure and its unit tests stand.

**No longer a blocker, and it does not need its own plan.**

**Verification.** `cargo xtask capture` on Line and Dots, to confirm the debounced respawn does not
destabilise the deterministic capture harness. Then a human running `cargo rund` pressing F11.

**Blocked by.** Nothing. **Soft-blocks** Plan 03 (fullscreen looks broken without it) and **touches
`cymatics/mod.rs`**, which Plan 07 also touches.

---

### Plan 03 — Fullscreen and display settings

**Goal.** Make the app usable as an unattended kiosk without a keyboard.

**Why.** `crates/waveconductor/src/main.rs:55` creates a 1280×720 **windowed** window. The only path
to fullscreen is the F11 keybind at `crates/wc-core/src/lifecycle/action_map.rs:104`, which a field
tester has no way of discovering. Nothing re-asserts fullscreen when a monitor re-enumerates — a TV
that sleeps and wakes drops the installation out of fullscreen for the rest of the night.

**Anchors.**
- `crates/waveconductor/src/main.rs:55` — `resolution: (1280, 720)`, no `mode` field, so `WindowMode::Windowed`
- `crates/wc-core/src/lifecycle/action_map.rs:104` — `(A::ToggleFullscreen, Key(KeyCode::F11))`
- `crates/wc-core/src/lifecycle/nav.rs:66` — `WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current)`

**Decisions locked (Madison chose the flat-checkbox shape over a first-class Kiosk mode).**
New `DisplaySettings` section owning: `start_fullscreen` (default `true` in release, `false` under
`debug_assertions` so `cargo rund` stays sane), `hide_cursor`, `monitor` persisted by
name with fallback to `Current`. A startup system applies the mode. The existing F11 handler writes
`start_fullscreen` back on toggle. Fullscreen and monitor are **re-asserted when the monitor set
changes** — that is the bit that survives a sleeping TV.

**Correction, 2026-07-09: `MonitorAdded` / `MonitorRemoved` do not exist.** This index and the spec both
asserted Bevy 0.19 fires them. It does not — neither symbol appears anywhere in `bevy_window-0.19.0/src`
or `bevy_winit-0.19.0/src`. Monitors are plain `Monitor` ECS entities, spawned and despawned by
`bevy_winit::system::create_monitors` (`bevy_winit-0.19.0/src/system.rs:177`) once per event-loop
iteration, with no dedicated event. Caught by Plan 03's author reading the vendored source instead of
trusting this document. Use `Added<Monitor>` / `RemovedComponents<Monitor>` to track the list, and make
the mode-applying system idempotent, writing `Window::mode` only when it differs from the target so
change detection does not thrash winit every frame.

**`boot_into_attract` is cut. Do not implement it.** The spec assigned it to the
`configurable-attract-mode-timeout` worktree, which in fact shipped only `attract_mode_timeout_secs`
(`lifecycle/screensaver/settings.rs:103`). An earlier draft of this index therefore reassigned it
here. Both were wrong about *why* it existed. It was never a requirement: the field tester asked that
Cymatics not launch blank, Madison proposed booting into attract mode as the mechanism, and he agreed
to the mechanism. The mechanism does not work (see Plan 07), so it goes, and the requirement is met
by Plan 07 instead. The residual kiosk-boot gap is recorded in Part 4.

**Blocked by.** Plan 02 in practice. Startup fullscreen with no resize handling means every kiosk
boots into the "framed fullscreen" bug the tester already reported. Shipping 03 without 02 would
make the product *look* worse.

---

### Plan 03a — Runtime-enumerated setting widget (prerequisite for 03 and 04)

**Goal.** One `SettingKind` whose options are supplied at runtime, so a monitor list and an audio-device
list can both be dropdowns instead of hand-typed strings.

**Why it exists as its own plan.** Discovered 2026-07-09 while checking Plan 04's open spike. Plan 03
(monitor picker) and Plan 04 (audio-device picker) each need it, and building it touches shared files:
`settings/def.rs`, the `SketchSettings` derive macro, and `settings/panel_user/widgets.rs`. If both plans
grow it independently we get two incompatible widgets and an ugly merge. Extracting it is what makes 03
and 04 genuinely parallel afterwards.

**What exists today.** `SettingKind` (`settings/def.rs:10`) has `Number`, `Boolean`, `Color`, `Text`,
`TextList`, `FilePath`, a templates-backed picker, and `Enum { variants: &'static [&'static str] }`
(`:54-60`). The `Enum` arm is a *compile-time* variant list, written back through reflection as a
payload-less `DynamicEnum` (`settings/commands.rs`), so it only supports unit-variant Rust enums. It is
the right precedent to copy and the wrong tool for this job.

**Shape.** The stored value is a `String` (the device or monitor name), so persistence needs no change —
it is a TOML string like `Text` already is. What is new is only the *widget*: a `ComboBox` whose options
come from a runtime source, plus a free-text escape hatch so a saved name that no longer resolves is
visible and editable rather than silently reset.

**Decisions to lock while writing this plan.**
- How options reach the widget. A `Resource` the panel reads (e.g. `AvailableAudioDevices(Vec<String>)`)
  keeps `SettingDef` static and the derive macro untouched, which is the cheapest path. Prefer it over
  putting a function pointer in `SettingDef`.
- What happens when the persisted name is absent from the live list. Show it, mark it unavailable, keep
  it persisted. Never silently rewrite the operator's choice — an HDMI TV that is merely asleep must not
  lose its saved binding.

**Blocked by.** Nothing. Small, self-contained, and **it gates the UI halves of 03 and 04.**

---

### Plan 04 — Audio output device selection and recovery

**Goal.** Sound comes out of the TV, and keeps coming out of it for eight hours.

**Why.** `crates/wc-core/src/audio/engine.rs:141` takes `cpal::default_host().default_output_device()`.
There is no enumeration and no picker anywhere in the crate — that single call is the only cpal
device call in the tree. Whatever Windows calls "default" gets the audio, which is why it never
reached the tester's TV over HDMI.

**The half that matters more.** `crates/wc-core/src/audio/state.rs:185` logs, verbatim,
*"Status set to Errored. Restart the app to recover audio."* and means it. There is no reopen path.
A single HDMI-audio endpoint blip — a TV sleeping, an input switch — silences an unattended
installation permanently.

**Decisions locked.** Enumerate `host.output_devices()`, persist the choice by name, resolve at
startup with fallback to the system default. Then a supervisor replaces the terminal
`AudioStatus::Errored`: the existing cpal error-callback flag triggers a stream rebuild with backoff
(1 s, 2 s, 4 s, capped at 30 s), and a background poll (~2 s) watches device topology so the stream
migrates back when the saved endpoint reappears. Rebuild re-resolves the device, recreates the
stream, and restores play/pause from `AppState`. `AudioStatus` gains `Reconnecting`.

**Hard constraints.** Enumeration and rebuild run off both the audio callback and the render thread —
WASAPI enumeration can block. The audio thread's real-time contract is unchanged: lock-free ring
buffers only, no `Mutex`, no allocation after init.

**Spike resolved 2026-07-09: it does not, and this collides with Plan 03.** `SettingKind::Enum` exists
and renders as an `egui::ComboBox`, but its options are `variants: &'static [&'static str]`
(`settings/def.rs:59`), filled by the derive macro from the field type's `TypeInfo` at compile time
(`enum_variant_names`, `def.rs:63`). `cpal` discovers devices at runtime. **There is no widget for a
runtime-enumerated list.** Plan 03's `monitor: Option<String>` needs the identical widget. See Plan 03a,
which both consume.

**Cross-plan risk found while authoring, 2026-07-09: a reconnect may come back silent.** The rebuilt
`DspHost` has no synth graph, because each sketch issues its `Add*Synth` commands only on
`OnEnter(AppState::That)`. So a mid-sketch reconnect can restore `AudioStatus::Running` and still emit
nothing — which would make this plan fail its own goal. Two ways out:

- **(a) Re-enter the sketch.** Plan 02 introduces `ReloadReason::WindowResize`, a *silent, instant*
  reload (one black frame, master volume untouched). An `AudioDeviceReconnect` reason would reuse the
  same primitive: re-running `OnEnter` re-adds the synth graph and restores play state for free. Costs a
  dependency on Plan 02 and one black frame on reconnect.
- **(b) Re-issue the synth commands from the supervisor**, which requires it to remember what was added.

Resolved in favour of (a), as the plan's **conditional Task 5R** (`ReloadReason::AudioDeviceReconnect`,
same zero-fade / no-audio-touch policy as `WindowResize`). Blocked on Plan 02, which introduces
`ReloadReason`. (b) was rejected because a supervisor replaying `Add*Synth` duplicates each sketch's
source of truth and drifts the moment a sketch changes; re-entry replays nothing and cannot drift.

**Task 5R is gated on a human answer and must not be built speculatively.** Plan 04's Task 5 Step 7 is a
`cargo rund` unplug/replug test whose sole job is to report *audible vs. silent* after reconnect. Audible
→ delete 5R, it was never needed. Silent → implement it. Do not let an implementer skip the gate.

**Blocked by.** Plan 03a (the widget). The audio *recovery* half — supervisor, backoff, `Reconnecting`
status — touches no UI and can land first.

---

### Plan 05 — Windows thermal sensor chain

**Goal.** Stop being thermally blind on the deployment hardware class.

**Why.** `crates/wc-core/src/lifecycle/thermal/platform/windows.rs:74` degrades to the
`Cool`/`Schedule` fallback when no WDDM adapter reports a temperature — which is what the tester's
Vega 10 does. The tier then pins to `Cool` forever, and `Cool` selects the **no-reduction** branch of
the screensaver present-rate throttle, the only thermal lever the app has. The failure is logged at
INFO, so nobody notices.

**Decisions locked.** Chain: existing **WDDM D3DKMT** (direct GPU-die temp) → new **WMI ACPI thermal
zone** → **Schedule**. `ThermalTier` and its hysteresis are unchanged. No `GpuTimeProxy` rung (the
enum variant stays declared and unbuilt).

**Why WMI is the right second rung, not a consolation prize.** WaveConductor is GPU-bound and its
only lever reduces GPU work, so GPU temperature is the signal in the abstract. But both deployment
candidates — Vega 10 and Radeon 780M — are APU iGPUs sharing one die, one power budget and one
thermal budget with the CPU cores. The package temperature ACPI zones expose is therefore a faithful
proxy, and it is available precisely where the WDDM query returns 0.

**Implementation notes.** Read `Win32_PerfFormattedData_Counters_ThermalZoneInformation` from
`root\CIMV2` (generally readable **without elevation**) in preference to
`MSAcpi_ThermalZoneTemperature` in `root\WMI` (often needs admin). Verify on the target box rather
than assuming. New `windows` crate **features** only (`Win32_System_Com`, `Win32_System_Wmi`,
`Win32_System_Variant`) — no new dependency; the crate is already optional behind
`thermal-sensor-windows` (`crates/wc-core/Cargo.toml:65`). Filter zones to a plausible 1–150 °C after
the Kelvin conversion; the hottest plausible zone wins. COM must be initialised per-thread; the query
lives on the existing thermal sampler thread, never the render thread.

**Also.** Escalate the "no sensor" line from INFO to **WARN** — silent thermal blindness is how this
went unnoticed. Log provenance (`source=wmi-zone zone="…" temp=61.3C`) and sample periodically, so
the next soak log carries the data needed to tune thresholds.

**Explicitly not in scope.** Threshold tuning. The current values are documented placeholders pending
real-hardware soak data and cannot honestly be tuned from a dev machine. Ship the sensor and the
logging; tune later. Note the behaviour change: a box that reports temperature will start classifying
real tiers against placeholder thresholds, where it previously sat at `Cool` forever. That is the
intended direction (throttling a cold machine is the safe failure), but watch it in the first soak.

**Caveat to record in code.** ACPI zone semantics are OEM-defined; some boxes report a chipset or
chassis-skin temperature rather than the die, so thresholds tuned on one machine do not transfer.
Skin and package temps lag die temps by tens of seconds — acceptable, because the lever is an
attract-mode present rate, not frame-level control.

**Testability.** Put the zone filtering and hottest-zone selection behind a trait so the selection
logic is unit-testable without a WMI call.

**Blocked by.** Nothing.

---

### Plan 06 — ONNX execution-provider resilience

**Goal.** A failing GPU execution provider must not cost the app all hand tracking.

**Why.** `crates/wc-core/src/input/providers/mediapipe/inference_ort.rs:98` propagates a DirectML
failure from `commit_from_memory` as a fatal `InferenceError::Load`. The guard at `:208-217` catches
errors only from `register()`, not from commit. DirectML *registers* fine (the Vega 10 is a valid DX12
device), then throws `80004005` inside `DmlGraphFusionHelper` during graph fusion. With no Leap device
attached, Windows therefore has **no hand tracking at all** — the chain falls through to
`MockProvider`.

The doc comments at `inference_ort.rs:6-9` and `:62-63` claim ONNX Runtime "falls back to CPU for any
op the EP cannot place, so load never fails closed." That conflates per-op placement fallback with an
EP crashing at commit time. **It is the assumption that produced the bug.** Correct it.

**Decisions locked.** `OrtInference::load` gains a commit-level retry: attempt the DirectML-configured
builder and `commit_from_memory`; on error, log a warning naming the failing model, construct a
**fresh** CPU-only `SessionBuilder` (skipping both `configure_accelerator_session` and
`register_accelerator`), commit that, return `BACKEND_CPU`. The builder is consumed by the failed
commit, hence the rebuild; the cost is paid only on the error path. Apply the same shape on macOS,
since CoreML commit can also fail.

**DirectML is still used wherever it commits.** This changes only failure behaviour.

**Per-model fallback is already free.** `combined_backend(palm, landmark)`
(`mediapipe/mod.rs:512`) takes two backend labels, `BACKEND_DIRECTML_CPU` ("ort/DirectML+CPU") already
exists, and `load_model` is called once per model (`mod.rs:276` palm, `:277` landmark). So if only
`palm_detection.onnx` trips DML fusion, `hand_landmark.onnx` keeps running on DirectML. Someone
anticipated this. There are already tests at `mod.rs:792,800`.

**Also.** Add a `backend: Auto | ForceGpu | ForceCpu` field to the existing
`crates/wc-core/src/settings/hand_tracking.rs` section, so the field tester can A/B without a new
build. Log the effective per-model backend at startup, and on EP failure the exact failing node, so
his existing "upload the log" workflow carries the diagnostic.

**Blocked by.** Nothing. Owns `input/providers/mediapipe/` and `settings/hand_tracking.rs`.

**Relationship to Plan 08.** 06 is the safety net that makes it *safe to investigate*. 08 is the
attempt to keep the GPU. They are independent and 06 ships regardless of what 08 finds.

---

### Plan 07 — Cymatics warm start

**Goal.** Cymatics must not look like a blue screen of death when a visitor cycles into it.

**Why.** `crates/wc-sketches/src/cymatics/mod.rs:389` (`init_cymatics_state`) runs on
`OnEnter(AppState::Cymatics)`, and the ping-pong textures are allocated fresh and zeroed **on every
entry**. So the blank field appears each time someone navigates to Cymatics, not once at boot.

The field tester: *"One small but significant tweak for Cymatics — have it show the two orange blobs
right away, so folks don't think it's a blue screen of death **when cycling thru**."* His log shows
`navigate target=Cymatics` at 23:22:42, 23:22:58, 23:23:23 and 23:23:31 — four cold starts in under a
minute.

**Why this is a warm start and not "enter attract mode."** Attract mode was the originally proposed
mechanism. It is the wrong one, for three reasons, and the reasons are worth keeping because they are
not obvious:

1. **It does not fix the reported bug.** Attract-on-boot changes what is on screen at t=0. The tester
   said *"when cycling thru."* It does not change what a partygoer sees the fifth time they press z/x.
2. **It cannot work without a cooldown, structurally.** `advance_activity` (`lifecycle/idle.rs:403`)
   is the sole writer of `NextState<SketchActivity>` and recomputes its target from the idle timer
   every frame; a competing writer produces the show↔hide flap that `apply_force_screensaver`
   (`screensaver/mod.rs:459-470`) exists to avoid. Worse, **the navigation input is itself an
   interaction**: the keypress or picker click that entered Cymatics lands in `reset_on_interaction`,
   which calls `timer.mark()` unconditionally (`idle.rs:232-242`), and even key-*up* events count.
   `skip_to_screensaver`'s doc records the resulting bug verbatim (`idle.rs:305-316`): *"A one-shot
   rewind on `just_pressed` would be cancelled by those releases and the screensaver would flash and
   wake."* Shift+S survives only by staying **armed** until the keyboard goes quiet. So any
   attract-on-launch needs a window where interaction is ignored.
3. **The required cooldown is what makes it hostile.** `SketchActivity::Screensaver` also throttles
   the present rate to 20 fps (`apply_present_rate`), closes the settings panel (`mod.rs:152`), and
   silences audio (attract is *"intentionally silent"*, `cymatics/screensaver.rs:222`). A visitor who
   taps the Cymatics tile would get a silent 20 fps screensaver that ignores their first touch, then
   lurches into `Active`. That is worse than the blank screen.

None of the three effects that make attract mode *look* good require being in the screensaver state.
Do them directly, in `Active`, on entry.

**Decisions locked.** Seed `CymaticsState` on `OnEnter(AppState::Cymatics)` and let the normal
`Active` systems take over. No state-machine change, no cooldown, no throttle, no silenced audio, and
it fixes **every** entry rather than only boot. Self-contained in `crates/wc-sketches/src/cymatics/`.

**The three seed parameters.** Each corresponds to one independent cause of the blankness:

| Field | Default | Why it is blank | Seed |
| --- | --- | --- | --- |
| `center` / `center2` | both `(0.5, 0.5)` (`mod.rs:129-130`) | the two centres **overlap**, so a bloomed mask shows one blob, not the two the tester asked for | `wander_centers(0.0, &LissajousSpeeds::from_settings(&settings))` |
| `active_radius` | `MINIMUM_ACTIVE_RADIUS` = `0.1` (`mod.rs:63,131`) | the resting alive-mask; `settings.rs:422` says `0.1` is *"a nearly invisible mask"* | toward `attract_radius` (default `1.0`) |
| `ramp_time` | `0.0` (`mod.rs:135`) | the shader's alive-bloom ramp is `(time-500)/500`, still below its foot; advances `N·dt`/frame, capped at `RAMP_TIME_CAP` = `1000` (`mod.rs:75`) | past the ramp foot |

`wander_centers` (`cymatics/screensaver.rs:81`) is pure, already unit-tested, and at `t=0` returns
`(0.5, 0.8)` and roughly `(0.80, 0.75)` — precisely the two separated blobs. Reuse it; do not
hand-roll coordinates.

**Do not try to reuse the raindrop scheduler.** `drive_cymatics_pings` is gated
`in_screensaver(AppState::Cymatics)` (`screensaver.rs:243`), so in `Active` the source is the
continuous oscillator at `center`, not raindrops. "Launch with a ring already expanding" is therefore
not free — it would mean seeding the texture directly. Letting the oscillator build from a
pre-bloomed, two-centre mask is far cheaper and probably looks right.

**Task 1 is a spike, and a human runs it.** Seed the three fields, run `cargo rund`, enter Cymatics
from the picker several times, and pick the values by eye. Only then write the test against what
landed. Do **not** derive `ramp_time`'s seed on paper and assert a magic number: there are no GPU
tests in CI (Part 1), and `cargo xtask capture` returns black frames for a backgrounded window, so an
agent cannot verify this. Madison must look at it.

**Blocked by.** Nothing, but it touches `cymatics/mod.rs`, which **Plan 02 also touches** (sim-grid
re-init on resize). Land 02 first, or coordinate the two.

---

### Plan 08 — DirectML remediation ladder (separate branch)

**Goal.** Keep the iGPU accelerated rather than settling for CPU. CPU is the floor, not the plan.

**Branch.** `windows-directml-prelu-rank`, branched from the remediation line. The whole
investigation — probe tool, model override, graph-opt flags, and any candidate fix — lives there,
self-contained, because the model swap could regress Windows further and rank-4 slopes would
definitely regress CoreML if they leaked to macOS. **Nothing speculative merges until the probe
returns data.**

**Tool first.** `cargo xtask probe-ep --model <path> --ep directml|coreml|cpu [--graph-opt level1|level3|disable] --json`.
Reports success or the exact error, node-placement counts per EP, partition count, and mean inference
latency. Crucially, **`xtask` depends on neither Bevy nor `wc-core`** (its deps are clap, ignore,
regex, image, serde, serde_json, toml — `xtask/Cargo.toml`), so this builds in minutes on a fresh
Windows clone. Add `ort` to `xtask` reusing the workspace pin (`=2.0.0-rc.12`); that is not a new
dependency.

**Rung 0 — the PRelu-rank experiment. This is the whole reason the plan exists.**

Static analysis of the vendored models (run on macOS, no Windows needed):

| Model | PRelu nodes | Slope shape | Rank | Pad / Resize / Concat | Conv |
| --- | --- | --- | --- | --- | --- |
| `palm_detection.onnx` (shipped) | 26 | `(C,1,1)` | 3 | 3 / 2 / 2 | 53 |
| `palm_detection_original.onnx` | 26 | `(1,C,1,1)` | 4 | 3 / 2 / 2 | 53 |
| `hand_landmark.onnx` | 0 | — | — | 0 / 0 / 0 | 47 |

The two palm variants are otherwise identical. The **sole** delta is the slope rank, introduced in
commit `d2369f4f` **for CoreML**, whose NeuralNetwork EP requires `[C,1,1]` or a scalar and rejects
`[1,C,1,1]` (`docs/runbooks/onnx-coreml-model-surgery.md:123-125`). That surgery predates any Windows
GPU-inference build. DirectML's operators are rank-specific, and `DmlGraphFusionHelper` is the
partitioner that must place `PRelu` between 53 convolutions. `mod.rs:276-277` loads palm **before**
landmark, and the tester's log shows exactly one initialization exception before the provider bails.

**Hypothesis: the CoreML fix is what broke DirectML.** Not proven — nobody has shown DirectML rejects
rank-3 slopes. What *is* established is that the sole difference between the failing model and its
unmodified upstream original is a rank change made for an unrelated platform, and that the clean model
carries none of the suspect ops.

Because the hypothesised trigger is device-independent, **Madison's RX 6900 XT (discrete RDNA2) is a
valid test bed**, even though it is not the tester's hardware:

| Result on the 6900 XT | Conclusion |
| --- | --- |
| Surgered palm fails, original palm passes | PRelu rank confirmed. Fix is free: `cfg`-select rank-4 on Windows, rank-3 on macOS. Both models keep full DirectML acceleration. `palm_detection_original.onnx` is already committed. |
| Both palm variants fail | It is Pad/Resize/Concat. Model-level, fixable locally, no field tester needed. |
| Both pass | GCN5- or driver-specific. Cheap causes excluded; escalate to the tester's box. |

**Remaining rungs, cheapest first, each falsifiable by the probe.**

1. Ship a newer `DirectML.dll`. The staged DLL is whatever pyke's `ort` rc.12 built against. DirectML is
   independently redistributable and fusion fixes for older GCN hardware land in it. **Anchor corrected
   2026-07-09:** the staging loop is `xtask/src/bundle/windows.rs:136-164` (the `is_directml` predicate at
   `:157`). An earlier revision of this index cited `:244`, which is a `runtime_dlls: vec!["DirectML.dll"]`
   test fixture literal, not the staging code. Caught by Plan 08's author.
2. Lower the graph optimization level (`Level1`, then `Disable`). ONNX Runtime issue #12538 suggests
   this may not rescue DML, but it is one flag and instantly falsifiable.
3. `optimization.disable_specified_optimizers` against named transformers.
4. A different ONNX Runtime build. `ort` statically links pyke's ORT; given the documented 16.3 → 17.0
   DML initialization regression (issue #21205), testing Microsoft's official `onnxruntime.dll` via
   `load-dynamic` establishes whether the failure is build-specific.
5. Model surgery, DirectML edition. Only if 0–4 fail.

**Keep the latency column in view.** On a shared-die APU, DirectML inference contends with the renderer
for the same shader cores and the same memory pool, and the renderer is already the bottleneck. CPU
inference on two ~4 MB models may be faster end-to-end than DirectML on a Vega 10. Winning this ladder
and then measuring a regression is a real possible outcome.

**Work split.** Everything except the probe *run* happens on macOS, including writing `probe-ep` and
exercising it against CoreML and CPU so it is known-good before it sees Windows. Rungs 1–3 are wired as
**flags**, not rebuilds, so a single Windows build tests every rung. The Windows box does one thing:
clone, `cargo run -p xtask -- probe-ep --ep directml --json` against all three models. The field tester
validates the outcome; he does not iterate.

**Blocked by.** Nothing in code. Blocked by **access to a Windows machine** for rung 0's probe run.

---

## Part 3 — Dependencies and parallelism

### The graph

```
     ┌──────────────────────────┐        ┌──────────────────────────┐
     │ 02 resize invalidation   │        │ 03a runtime-enum widget  │
     │ (+ egui panel, resolved) │        │ (shared settings widget) │
     └───────────┬──────────────┘        └───────┬──────────┬───────┘
       soft      │      file                     │          │
       block     │      overlap                  │ UI half  │ UI half
  ┌──────────────┴──────────────┐                │          │
  ▼                             ▼                ▼          ▼
┌──────────────────┐  ┌────────────────────┐  (03)      ┌──────────────────┐
│ 03 fullscreen +  │  │ 07 cymatics warm   │            │ 04 audio device  │
│ display settings │  │ start              │            │ + recovery       │
└──────────────────┘  └────────────────────┘            └──────────────────┘
  ▲ also needs 03a                                        ▲ recovery half
                                                            needs nothing

  fully independent, no shared files:
        ┌──────────────────┐  ┌──────────────────┐
        │ 05 thermal WMI   │  │ 06 ONNX EP       │
        │ chain            │  │ resilience       │
        └──────────────────┘  └──────────────────┘

  separate branch, needs a Windows box:
        ┌──────────────────────────────────────────┐
        │ 08 DirectML probe + PRelu-rank experiment│
        └──────────────────────────────────────────┘
```

### Why so much is parallel

Because settings are decentralised (Part 1), the plans that add operator-facing knobs each own a
different module:

| Plan | Owns | Settings section | Needs 03a? |
| --- | --- | --- | --- |
| 03 | `waveconductor/src/main.rs`, `lifecycle/nav.rs`, new display settings module | `DisplaySettings` (new) | **yes** — monitor picker |
| 04 | `wc-core/src/audio/` | `AudioSettings` (new) | **yes** — device picker |
| 06 | `input/providers/mediapipe/` | `settings/hand_tracking.rs` (existing) | no — `Auto\|ForceGpu\|ForceCpu` is a static unit-variant enum, which `ty = Enum` already handles (see `HandProviderChoice`, `hand_tracking.rs:59`) |

**Correction, 2026-07-09.** An earlier revision of this section claimed no two of them write the same
file. That was wrong for the 03/04 pair: both need a runtime-enumerated dropdown, which does not exist
and must be built in shared files (`settings/def.rs`, the derive macro, `panel_user/widgets.rs`). Hence
Plan 03a. The underlying observation still holds — the spec's assumed monolithic `settings.rs`
merge-conflict hotspot genuinely does not exist, and 06 collides with nobody.

### Writing the plans vs. implementing them

These parallelise differently and the distinction matters.

**Writing plan documents is fully parallel.** Seven markdown files in `docs/superpowers/plans/`, disjoint,
**no cargo builds**. Build contention is the whole cost of parallel work on this machine; doc-writing has
none. Every plan-writing agent must be handed Part 1 of this index, or each will independently rediscover
the `--all-targets` clippy gap and the doc gate's private-intra-doc-link rule.

**Implementing is serial.** Three constraints stack and all point the same way:

1. No concurrent cargo builds (`target/` exceeds 40 GB; the data volume runs near full) and no worktrees,
   for the same reason. So parallel implementers cannot each verify their own work.
2. `subagent-driven-development` forbids parallel implementation subagents outright (conflicts).
3. **Plan 01's three real defects were each caught by a review gate on a small isolated diff** — the
   vacuous leak-regression test, the AGENTS.md citation of a nonexistent path, and the blur that silently
   never drew. The last was invisible to *every* automated gate, because there are no GPU tests in CI. A
   batched build at the end of five parallel plans would have returned green on broken rendering.

Serial is also less slow than it looks: 02 is the long pole and it unblocks the two plans behind it.

### Recommended order

**Wave 1 — 02 and 03a.** Both are prerequisites and neither blocks the other.

- 02 is the highest value: three of the tester's reports, one root cause, and it unblocks 03 and 07.
- 03a is small and gates the UI halves of 03 and 04.

**Wave 2 — 05, 06, 04, 07, 03.** After Wave 1, these are mutually independent.

- 05 and 06 touch nobody else and could have gone in Wave 1; they sit here only because implementation is serial.
- 04's *recovery* half (supervisor, backoff, `Reconnecting`) needs no UI and does not wait on 03a.
- 07 edits `cymatics/mod.rs`, which 02 also edits for the sim-grid re-init.
- 03 must follow 02: startup fullscreen without resize handling ships the "framed fullscreen" bug to every kiosk boot.

**Alongside, on its own branch — 08.** Its real cost is a probe run on the Windows box, so it never
competes for this machine's build slot. Write `probe-ep` on macOS now.

03 and 07 were once entangled through `boot_into_attract`. They are not any more: cutting that feature
left 07 self-contained in `crates/wc-sketches/src/cymatics/`. Their only remaining relationship is that
both sit behind 02.

### The one genuine blocker

**Plan 08's rung 0 needs a Windows machine.** Everything else on 08 is macOS work.

Plan 02's egui panel bug is **no longer a blocker** — spiked and root-caused on 2026-07-09; see Plan 02.

### What is *not* a blocker, despite appearances

- The `configurable-attract-mode-timeout` worktree has already merged (`5ea5d338`). The spec's
  sequencing constraint is discharged.
- Plan 06 and Plan 08 do not block each other. 06 ships the safety net; 08 chases the GPU.
- Plan 04's audio *recovery* work needs no UI at all and can land before the picker question is settled.

---

## Part 4 — Still unowned

- **Kiosk boot gap (the residue of `boot_into_attract`).** A power-cycled kiosk comes up on
  `AppState::Home` (`lifecycle/state.rs:14-15`) showing a static picker, and sits there until someone
  touches it. Nobody has reported this; it is a real gap, but a different feature with a different
  trigger, and it is not what the Cymatics complaint was about. **If it is ever built, note the trap:**
  `SketchActivity` is a sub-state whose `#[source]` covers the five sketches but **not** `Home`
  (`state.rs:115-117`), so at Home there is no attract mode to boot into and `advance_activity` returns
  early. "Boot into attract" therefore silently implies "boot into a *sketch*", and someone must choose
  which — first-in-cycle, a configured pick, or last-used. Prefer the first two; persisting the last
  sketch across launches adds a new state surface to a machine that gets power-cycled. No cooldown is
  needed here (unlike attract-on-navigation): at boot nothing has been interacted with, and the existing
  `force_screensaver` flag (`idle.rs:43`) is checked ahead of the elapsed-time thresholds
  (`idle.rs:409-411`), so it holds until the first real interaction clears it.
- **Instruction-screen overlay.** Never built. Madison's note: *"Instructions should appear as an
  overlay at the bottom with an image of the head/sensor and showing the hands waving."* Needs a design
  pass, not just a plan. Not on the alpha.5 critical path.
- **Three upstream Bevy / `bevy_egui` issues.** File as issues, not PRs.
  1. `bevy_egui`'s `EguiRenderTargetData::target_size` is declared, zero-initialised, read into every
     paint callback's `update()` hook, and never assigned (found in Plan 01).
  2. `TonemappingBindGroupCache` never hits when the per-frame count of `post_process_write()` calls is
     odd (found in Plan 01).
  3. `update_ui_screen_rect` (`bevy_egui/src/lib.rs:1868`) derives `screen_rect` from
     `egui_output.pixels_per_point`, which is written *after* the egui pass from the previous frame's
     `FullOutput` (`output.rs:56`) and defaults to `1.0`. So `screen_rect` lags the scale factor by one
     frame, even though the current value is already available as
     `egui_input.viewports[ROOT].native_pixels_per_point` (`input.rs:1247`). Visible on any
     `pixels_per_point != 1.0` display at startup and on every scale-factor change. Found in Plan 02's
     spike, 2026-07-09.
- **VRAM budget telemetry** (`IDXGIAdapter3::QueryVideoMemoryInfo`) — explicitly deferred.
- **`cargo xtask soak-test`** — planned, not implemented. Do not cite it as if it exists.
- **Thermal threshold tuning** — gated on a real soak log from the deployment hardware.
