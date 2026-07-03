# WaveConductor v5 roadmap

WaveConductor v5 is the Rust/Bevy rewrite of the v4 React/Three.js generative-art gallery. The near-term goal is **parity with v4** (then better) for the unattended kiosk installation; the longer arc adds an iPad deployment, new sketches, and a web showcase.

This is the index. Detailed per-item plans live under `docs/superpowers/plans/`; the design spec is `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md`; per-item housekeeping accumulates in `docs/superpowers/next-plan-carry-forwards.md`. Work lands on the `v5-alpha` branch.

## How this roadmap works

- **Forward work is tracked as slugged items** (`kebab-case`), not numbered steps. Numbering bakes priority into every label and makes re-prioritising a rename cascade; slugs are stable identifiers you never renumber.
- **Ordering and dependencies live in one place — *Sequence & priorities* below**, which references slugs. Re-prioritising is editing that one list, not re-touching every item, commit, and comment.
- **Shipped work is a ledger** (*Shipped history*, near the bottom) — immutable record + tags. The detail lives in the per-plan docs under `plans/`.
- Discrete shippables close with a commit + a `v5-<slug>` tag; sketch-touching items also update `crates/wc-sketches/src/<sketch>/PARITY.md`.

## Kiosk interaction model (applies to every sketch)

The installation drives a **projector** — the kiosk's display is projected, not a touchscreen. So:

- **Touchless hand-tracking is the *primary* interaction.** On the desktop kiosk that's the Leap Motion Controller; on an iPad it's Apple Vision (camera-based).
- **Touch and mouse are *secondary* control modes** — supported, but not the main experience (you can't touch a projection).
- **Hand-Z is not required.** No current sketch's key interaction fails without depth (confirmed 2026-05-30), so 2D hand landmarks (e.g. Apple Vision without LiDAR fusion) are acceptable across the whole deck. 3D depth is a future enhancement, never a blocker.

## Deployment targets

**All three major desktop OSes are first-class deployment targets: macOS, Linux, and Windows** (plus WASM/WebGPU for the web showcase). Do **not** assume a single deployment OS — thermal sensing, crash recovery, and Leap-service management each need a per-OS backend, and none is "the" target. The iPad/iOS port (Phase 3) is **additive and exploratory**, not the primary device.

## Sequence & priorities

The orderable list. Five phases in priority order; within each, slugs roughly in dependency order. **Re-prioritise by editing here** — the slugs are stable, so nothing downstream renumbers.

**Phase 1 — Desktop v4 parity** *(optimise opportunistically; leave architectural hooks open)*
- 1.A Screensaver / attract / compute-saver / kiosk robustness: `screensaver-attract` · **`leap-deep-idle-state` (current main priority)** · `leap-watchdog-recovery` · `audio-device-failover` · `leap-sdk-archive`
- 1.B Port the remaining four sketches: `sketch-flame` → `sketch-dots` → `sketch-cymatics` → (`mic-fft` →) `sketch-waves`
- Close Line: `line-parity-signoff`
- Opportunistic / anytime (not phase-gated): `dev-velocity-build` · `thermal-seam-generalization` · `perf-soak-telemetry` · `soak-test-command`

**Phase 2 — Deep optimisation pass** *(after parity / better-than-v4)*
- `perf-soak-telemetry` → `frametime-percentiles` → `perf-governor` → `dynamic-resolution`

**Phase 3 — iOS / iPad port**
- `ios-foundations` → `ios-first-light` → `ios-thermal-soak` → `ios-vision-hands` → `ios-dynamic-resolution` → `ios-kiosk-hardening`

**Phase 4 — New sketches & major features**
- `face-body-tracking` · `new-sketches` · (other major features TBD)

**Phase 5 — Web showcase** *(lowest priority — v4 already covers web; this is portfolio, not the kiosk)*
- `web-showcase`

**Release gate — tag `v5.0.0`, merge `v5-alpha` → `main`** *(cuts across; see Release gates)*

Key dependencies:
- `sketch-waves` depends on `mic-fft` (microphone capture + rustfft, deferred from the audio scaffolding).
- `perf-governor` depends on `perf-soak-telemetry` + `frametime-percentiles` — you can't tune a governor without the data it reacts to.
- `dynamic-resolution` is evidence-gated on `perf-soak-telemetry`; its priority rises sharply if the iPad is chosen (fanless + retina).
- `ios-thermal-soak` depends on `thermal-seam-generalization` + `perf-soak-telemetry`; `ios-dynamic-resolution` depends on `dynamic-resolution` + the iOS-soak evidence.
- `ios-vision-hands` is **required** for the iPad kiosk (touchless is primary), not optional.

---

## Phase 1 — Desktop v4 parity

Bring v5 to parity (then better) with the v4 gallery on the desktop kiosk. Optimise opportunistically; leave hooks open for the later optimisation and iOS phases rather than building them now.

Line is essentially there — Plans 7–11.6 shipped the simulation, rendering, audio, overlay UI, and the Leap provider (see *Shipped history*). What remains in Phase 1 is the screensaver hardening, the four remaining sketch ports, and the Line parity sign-off.

### `line-parity-signoff`

*Was Plan 11.7.* The closing Line step: a manual side-by-side capture vs v4 (1280×720, idle / mid-press / mid-decay states, mouse + touch + Leap pinch exercised, audio recorded), roll the v5-only divergences into `crates/wc-sketches/src/line/PARITY.md` as approved deviations, flip the verdict PENDING → PASS, tag `v5-line-parity`. Deviations to record:

- **Multi-axis gyroscope** attractor visual (replaces v4's tilted single ring) — deliberate v5 design choice, Madison-directed.
- **Pad-instrument synth** (stochastic LFOs, pink-noise breath, configurable attack/release) — documents the universal audio-coupling pattern.
- **Heatmap-image spawn** accepts `png/jpg/jpeg/webp` (v4: png only).
- The 11.5 overlay-UI deviations (panel/button alpha bumps, backdrop-blur on buttons, fade-overlay reload, sheen rotation, HDR+AgX+bloom pipeline) — full list under *Shipped history*.

Mostly mechanical capture work; the substance shipped in Plans 11 / 11.5 / 11.6.

### `screensaver-attract` (1.A)

*Was Plan 11.8.* The screensaver / attract / compute-saver mode — the thing we've been building. Code shipped for three of four seams (thermal signal, screensaver framework, Line attract driver). Design spec: `specs/2026-05-29-line-screensaver-attract-mode-design.md` (§10 as-built addendum). As-built deviations: Hot tier = "Low-Rate Ember" (full pipeline at ~3 fps present rate), not a frozen dispatch; thermal sensing = zero-dep Linux sysfs reader, not `sysinfo`.

Remaining (deferred; none blocks a `v5-screensaver` tag on its own):

- **Thermal sensor sign-off + threshold tuning** *(per-OS — all three desktop targets first-class)*. Thermal sensing needs a backend per desktop OS: **macOS** = macmon (Apple Silicon, shipped 2026-05-30); **Linux** = zero-dep sysfs `hwmon` reader (compile/ABI-verified, not yet run against live `/sys` — confirm a `coretemp`/`k10temp` chip); **Windows** = no backend yet (now in scope — likely a WMI / `OpenHardwareMonitor`-class source). Tune the placeholder bands (`enter_warm 75` / `enter_hot 90 °C` in `lifecycle::thermal::mod.rs`) against observed throttle temps on each box. (On iPad this path is replaced by the OS thermal enum — see `thermal-seam-generalization`.)
- **Hot-tier "warm-up-then-freeze" escalation** *(deferred — YAGNI until evidence)*. If the soak shows ~3 fps + `leap-idle-pause` still runs the box too hot, add a true dispatch freeze that runs until particle alpha saturates, then latches `particle_count = 0`.
- **Presence-reactive attract layer** *(deferred)*. Particles lean toward an approaching visitor pre-engage; the seam is left open. (a) react to a hand entering the tracking volume (free); (b) tap raw Leap IR images (`IMAGES` policy) for near-field detection.
- **Per-sketch attract visuals.** The framework (`in_screensaver(AppState)` + per-tier present throttle + caption overlay) already supports them; only Line authored a performer. Flame/Dots/Cymatics/Waves each register their own when built.
- **Attract subtlety pass — bias the Line screensaver toward the quieter, more ambient end.** The shipped composition is "Autonomous Dreaming (subtle base) + Invitation Pulse (spine)" (spec D3). The new direction: make the *overall* attract read as calmer and less attention-grabbing — soften and/or space out the Invitation Pulse, let the resting-dream base dominate, and trim particle activity/brightness — while it still reads unmistakably as the real Line sketch. Pure choreography/tuning on the existing Seam 3 driver (`line/screensaver.rs`); no new pipeline and no thermal-tier change. Settle the feel via `cargo xtask capture line-screensaver` (review the resting-dream → pulse → release arc) and record the resulting stance in `line/PARITY.md`. *Deliberately re-weights the spec's "tripper trap" goal (§1 #2) toward ambient-and-subtle — an intensity dial-down, not abandoning attract.*

### `leap-deep-idle-state` (1.A) — **main priority**

*Was `leap-idle-pause` (CF #84). The naive "pause the Leap service during the screensaver" duty cycle was built and **live-tested 2026-06-03 — it wedged the device** (sustained `set_pause` toggling froze the tracking data path; recovery needed a USB replug).* Full as-built findings, the detection/recovery escalation ladder, and the least-privilege per-OS recovery architecture (no blanket sudo — Linux polkit/udev, macOS `SMAppService` one-verb helper, Windows SCM-ACL) live in **`specs/2026-06-03-leap-service-recovery-design.md`**.

Managing Leap state during deep idle is the critical lever for deep-idle perf, so this is the current top priority. It splits into:

- **Shipped (commit `e9831ab5`):** `reset_on_interaction` now ignores empty Leap frames, so a connected Leap no longer pins the idle timer (previously the screensaver never triggered on a real install). Independent correctness fix; kept regardless of the rest.
- **Held, on-branch (⚠ still wired):** the duty-cycle code (`6cc22231`, `b0d539d0`, `a0d72d5a`) is viable in isolation (a standalone harness at the aggressive setting does *not* wedge) but wedges the full GPU app. It is still registered behind the `hand-tracking-gestures` feature, so a `cargo rund` that idles 60 s **will wedge the Leap until replug** — kept active for now only to help reproduce the wedge. Gate it behind an off-by-default flag (or revert the wiring) before any unattended run.
- **Next:** replicate the wedge deterministically (lead: Ultraleap-documented GPU/concurrency contention — the CPU-only harness never wedged) → build detection (frame heartbeat + leaprs `ConnectionLost`) + the recovery ladder (client reconnect → USB reset → scoped service restart → reboot), first-class on macOS/Linux/Windows.
- ✅ Detection landed: `PrimaryState::DeviceWedged` drives the status LED + a `LeapWedgeChanged` message after ~4 s of attached-but-frozen silence (`input/wedge.rs`, `surface_leap_wedge`). Recovery (privileged restart / USB reset / alerting) is the remaining, separate increment — now tracked as **`leap-watchdog-recovery`** below.

Diagnostic harnesses: `crates/wc-core/examples/leap_pause_probe.rs` (resume latency median 32 ms / max 79 ms) and `leap_duty_stress.rs` (window sweep). *Leap path only — compiles out on iPad, which uses in-process Apple Vision and has no external service to wedge.*

### `leap-watchdog-recovery` — Ultraleap watchdog + self-heal (1.A)

*The recovery half of `leap-deep-idle-state`, promoted to its own tracked item. Detection already landed (`PrimaryState::DeviceWedged` + `LeapWedgeChanged`, `input/wedge.rs`); this is the **self-heal** increment — the watchdog that monitors the Ultraleap tracking service for the "alive-but-frozen" wedge and recovers it unattended over 12+ hour runs.* Full architecture, the empirically-tested recovery rungs, and the per-OS least-privilege mechanisms live in **`specs/2026-06-03-leap-service-recovery-design.md`**; the in-the-moment operator steps live in **`runbooks/leap-wedge-troubleshooting.md`**.

Why it's a standalone requirement (not just cleanup for our own duty cycle): a wedge was observed **spontaneously, with WaveConductor not running at all** (2026-06-03) — the vendor service freezes on its own under GPU/concurrency contention. So the kiosk's hand-tracking dependency can die over a long unattended run regardless of what we decide about pausing; detection + recovery is required either way.

- **Architecture — invert ownership (do *not* build a privileged supervisor).** The in-app side is an unprivileged **liveness watchdog**: the provider already stamps `last_tracking_instant` and flips `TrackingFlow::NotStreaming` after 1 s of silence while we expect streaming; the watchdog debounces that (+ corroborates with leaprs `ConnectionLost`) and *requests* recovery through a **narrow, OS-authorized channel** — never by holding root itself. "Watchdog *service*" here means the OS-supervised recovery verb (polkit / SCM-ACL / `SMAppService` XPC / udev), **not** a new always-on root daemon; a general-purpose privileged supervisor, `NOPASSWD: ALL`, and setuid wrappers are explicitly rejected in the spec.
- **Recovery escalation ladder** (cheapest / least-privilege first): rung 0 observe/debounce → rung 1 client reconnect (`stop()`+`start()`, in-app) → rung 2 USB device reset (re-enumerate) → rung 2b USB VBUS power-cycle (`uhubctl` + PPPS hub) → rung 3 scoped service restart → rung 4 reboot. Bounded retries, rate-limited, every action logged.
- **Per-OS least-privilege mechanisms.** *Linux* — a polkit rule scoped to *restart of only the Ultraleap unit by only the kiosk user* + a udev rule for the device-node USB reset + a systemd drop-in (`Restart=always`, `StartLimit*`, `StartLimitAction=reboot` as last resort). *macOS* — an `SMAppService` (13+) one-verb privileged helper exposing a single "restart-leap" XPC verb (pre-13: `SMJobBless`); dev-box interim is a sudoers line scoped to the exact `launchctl kickstart` command. *Windows* (now first-class — needs its own research pass) — a per-service DACL granting `SERVICE_START|SERVICE_STOP` on only that service + `pnputil /restart-device` for the USB reset.
- **Empirically settled (2026-06-03, live-tested via `leap_recovery_probe`):** rung 1 ✗ and rung 3 ✗ for a *device-session* wedge on macOS — only a **physical USB replug** recovered it. ⚠ **macOS has no fully-automated recovery for a device-session wedge** (no per-device USB reset); Linux (sysfs `authorized` / `usbreset` / udev) and Windows (`pnputil /restart-device`) *can* re-enumerate in software, so they don't share this gap. Implication: prefer Linux/Windows for an unattended kiosk if this wedge proves frequent, and prioritize *preventing* it (avoid GPU contention) since macOS recovery is manual.
- **Still open / on-device:** does rung 2 (USB reset) clear it on Linux/Windows; does rung 3 ever clear a *daemon-state* freeze caught before it becomes a device-session wedge; exact service/unit names per OS; the Windows SCM-ACL grant (dedicated research pass); whether `uhubctl`/PPPS actually power-cycles on macOS. Plus the **alerting** path — surface "needs physical replug" to the operator when the ladder is exhausted.
- *Leap path only — compiles out on iPad (in-process Apple Vision; no external service to wedge).*

### `audio-device-failover` — unattended audio survival + device selection (1.A)

*The audio analog of `leap-watchdog-recovery`: the installation must not run silent for hours if the output device disappears. Foundation shipped in the 2026-07 repo-audit fixes (AUDIT T8): a mid-run cpal stream error now propagates to `AudioState` as `AudioStatus::Errored` through a lock-free flag, instead of being swallowed by the error callback.* Two increments on that foundation:

- **Graceful failover (kiosk robustness).** On `Errored` (device unplugged / slept / format change), tear down and rebuild the cpal stream against the current default device, re-attaching the existing fundsp+rtrb graph and sample bank, with bounded retries and backoff — the install recovers audio on its own rather than needing a manual restart.
- **Device / driver selection (feature).** A DAW-style output-device (and, where the host exposes it, driver/API) picker in the settings UI, with the chosen device persisted; hot-unplug of the selected device falls back to default and surfaces the change.

Prior art: DAW audio-engine UX. cpal already enumerates devices; the lock-free ring + `Copy`-only command shape established for the synth graph is the template for rebuilding the stream without touching the real-time path.

### `leap-sdk-archive` — Ultraleap SDK availability hedge (1.A, ops)

*Belt-and-suspenders for the decision (AUDIT §6) to keep vendoring the LeapC binaries in-repo. Ultraleap is effectively abandonware; the real risk is not licensing but that the SDK **stops being distributed at all**.* Keep an **offsite archive of the original Ultraleap SDK installers** (all three desktop platforms + the exact 6.2.0 version the vendored `vendor/leapc/` binaries came from), outside this repo, so the vendored copies can be regenerated or re-verified if they're ever lost or corrupted. Pure ops, no code. Pairs with `leap-watchdog-recovery` (the *runtime* failure mode); this covers the *supply* failure mode.

### `sketch-flame` / `sketch-dots` / `sketch-cymatics` / `sketch-waves` (1.B)

The remaining four v4 sketches. Each ships its own `PARITY.md`, absorbs accumulated carry-forwards, and registers a screensaver attract performer. Per-sketch character (design spec §8 + the universal audio-coupling pattern):

| Slug | Parity target | Notes |
| ---- | ------------- | ----- |
| `sketch-flame` | Perceptual | **Shipped 2026-07-02** (`crates/wc-sketches/src/flame/PARITY.md`). IFS fractal; recognizability matters, chaotic detail can drift. Shipped as **GPU level-parallel IFS** — this supersedes the earlier "CPU-bound (no GPU parallelism)" characterization: the recursion is parallel *within* each tree level, so it runs one compute dispatch per level (5–16/frame) over a persistent node buffer, no per-frame CPU walk and no GPU↔CPU readback. Audio is envelope/DSP approximation from CPU input scalars (analytic `\|dcX/dt\|` + warp speed + camera distance), not visitor stats. |
| `sketch-dots` | Perceptual | Shares most infrastructure with Line. **Keep particles on CPU** (matches v4); only fall back to the approximated-envelope pattern if counts ever force a GPU port. |
| `sketch-cymatics` | Physics-matched | 2025-era human-authored; the visual *is* the simulation, numerical drift = wrong sketch. GPU compute (ping-pong wave PDE); audio reads CPU-side input scalars (`activeRadius`, `numCycles`, `centerSpeed`, `slowDownAmount`), never GPU state. The architectural reference for the universal pattern. |
| `sketch-waves` | Perceptual | Audio→visual coupling (FFT of microphone). Depends on `mic-fft`. Visuals are a closed-form CPU heightmap; no GPU compute. |

Order is provisional — the actual sequence depends on which sketch's data demands surface architectural gaps soonest.

> **Re-entry checklist (2026-07 audit, T5; Flame re-entered 2026-07-02).** `AppState::Flame` came back online on 2026-07-02 (registered plugin + manifest tile, re-added to `SKETCH_ORDER`, `SelectFlame`/`Digit2` binding restored, screensaver performer authored). **`AppState::Waves` remains the only de-routed seam:** its `SketchActivity` source still exists but is removed from `SKETCH_ORDER`, the picker, and the `Select*` bindings, and `WAVECONDUCTOR_START_SKETCH` falls back to Home for its name — so a stray keypress can no longer land on a black screen. Bringing Waves online is the reverse: register its plugin + manifest, re-add it to `SKETCH_ORDER`, restore its picker tile and binding, and author its screensaver attract performer. The "every `SKETCH_ORDER` entry has a registered manifest" test (added by T5, in `crates/wc-core/tests/lifecycle.rs`) is the guard — it fails if a variant re-enters the cycle unimplemented.

### `mic-fft`

Microphone capture + `rustfft` path, deferred from the audio scaffolding (Plan 4). Prerequisite for `sketch-waves` (audio is the *input* there, not the output).

### Opportunistic hooks (Phase 1, not phase-gated)

Cheap-and-compounding or leave-a-hook-open items that can land anytime during Phase 1 — they don't block parity but pay dividends across later phases.

- **`dev-velocity-build`** — `bevy/dynamic_linking` behind a dev-only flag/alias (never release/WASM; spike against the vendored-LeapC rpath first) + a fast linker (`mold`/`lld` on Linux, *appended* to the existing per-target `rustflags`; skip `sold`/`zld` on macOS — Apple's `ld` is already fast). CF #85 (**shipped 2026-05-30** — `cargo rund` alias; coexistence spike passed on Apple Silicon), #86 (open). Build profiles themselves are already done (`[profile.release]` fat-LTO + `codegen-units=1` + `panic=abort` + `strip`; deps at opt-3 in dev).
- **`thermal-seam-generalization`** — make the thermal sensor seam source-agnostic (`tier`-producing alongside `°C`-producing) so the Linux-sysfs/macmon °C path and the iOS `ProcessInfo.thermalState` enum path are two backends behind one `ThermalState`. The `ThermalState` *resource* already absorbs a new `ThermalSource`; the sensor abstraction doesn't (it's `read_celsius -> Option<f32>` all the way down). Worth doing regardless of device — it's the generalisation the v5 thermal thesis rests on. CF #90.
- **`perf-soak-telemetry`** — listed in Phase 2, but it's the gating evidence for both the optimisation phase and the iOS go/no-go, so it can be built opportunistically as soon as a candidate device exists.
- **`soak-test-command`** — convert the release-gate 8-hour soak from a manual procedure into an agent-operable `cargo xtask soak-test` subcommand (launch + duration + thermal / FPS / RSS logging + a summary verdict), following the harness's dispatcher + `--json` pattern. AGENTS.md / README now document the soak honestly as a manual procedure and flag this command as planned-not-implemented (audit T6); this item *is* that command. Overlaps `perf-soak-telemetry` (the full-render soak it would drive) and the Release-gate soak.

---

## Phase 2 — Deep optimisation pass

After v4 parity (or better), a focused optimisation pass. **Build order: soak telemetry → analyse → governor.** Triaged from an external performance-research pass (Perplexity) + an in-repo senior-engineer adversarial review. The headline was that most of the researcher's stack was already shipped, and the rest is YAGNI until a full-render soak produces evidence — the governor is a *retrofit against data*, not a greenfield architecture.

Already shipped (no action): three-tier-equivalent build profiles; zero-dep thermal sensing with hysteresis (enabled in the deployment binary); the screensaver's per-tier present-rate throttle (`UpdateMode::Reactive`, ≈30/15/3 fps).

### `perf-soak-telemetry`

The gating prerequisite. The current 8-hour soak runs `MinimalPlugins` (no RenderApp, no GPU) — the real renderer has never run unattended on hardware. Build a `DefaultPlugins` full-render, screensaver-resident soak on the candidate device, with **CSV/structured telemetry** (timestamp, frame-time mean/p95/p99, `ThermalState.tier` + `last_temp_c`, entity count). It either proves the present-rate throttle already holds thermals (most of the rest is then YAGNI) or produces the data that justifies the governor. The one artifact that serves both candidate devices and the iOS go/no-go. CF #87, #88.

### `frametime-percentiles`

Custom p95/p99 over `FrameTimeDiagnosticsPlugin` history (the built-in exposes a smoothed average only; ~30 LOC over the history ring, sized for a 12-hour run not the default ≈20 samples). Feeds the governor and the perf-audit harness.

### `perf-governor`

A `PerformanceGovernor` + `QualityLevel` / `QualitySettings` subscription pattern, **frame-time-primary with thermal as a secondary bias** (asymmetric hysteresis, mirroring the thermal tiers; sketches subscribe to the shared signal, cf. the universal audio-coupling pattern). Generalises the design-for-but-defer "in-sketch live thermal auto-adaptation" (spec D9) into a multi-signal governor that throttles **live** particle-count / dispatch-size / fps during play (today only the screensaver throttles, and only present-rate).

Frame-time-primary is the right *primary* signal because it's the only fine-grained adaptive signal that survives the worst-case targets: **WASM / WebGPU exposes no thermal sensor at all**, and **iOS exposes only a coarse 4-level enum**. Gated on `perf-soak-telemetry` + `frametime-percentiles`.

### `dynamic-resolution`

Render-scale DRS — the biggest GPU lever. Render the 2D + gravity-post pass into a reduced `Image` target and upscale on composite (Bevy's `MainPassResolutionOverride` is DLSS-oriented and excludes 2D/post passes, so it doesn't help Line — this is a real custom-pipeline change). UI stays native-crisp. **Priority is device-conditional:** evidence-gated/defer on an actively-cooled box; **likely-required on a fanless A12Z iPad at retina res** (a 264 ppi panel hides a ~0.5× particle/post pass). Reject `bevy-dynamic-viewport` and `set_scale_factor_override` (rescales UI too).

*Dropped from the research:* the sysinfo/wmi/macmon thermal-crate survey (superseded by the zero-dep reader); Intel-Mac thermal completeness (not a target) — **but Windows thermal IS now in scope (Windows is a first-class desktop target); see the per-OS thermal sign-off item**; an in-app **perf-governance** watchdog (superseded by the frame-time-primary `perf-governor` — distinct from the Leap-service `leap-watchdog-recovery` in Phase 1, which *is* in scope); `bevy-dynamic-viewport`; `set_scale_factor_override` DRS; a shared `RenderScale` resource sketches "inherit" (2D vs 3D passes don't share a target shape — retrofit per-camera instead).

---

## Phase 3 — iOS / iPad port

An **additive, exploratory** target — *not* a primary device; the three desktop OSes (macOS / Linux / Windows) are the first-class deployment targets (see **Deployment targets** above): **iPad Pro 11″ 2nd gen (MY232LL/A) — A12Z (2020), 8-core GPU, fanless, LiDAR + TrueDepth, 2388×1668 retina (264 ppi) 120 Hz ProMotion, iPadOS 26.** Added alongside the four stated build targets (macOS, Linux, Windows, WASM all keep compiling); iOS is additive. Triaged from a Perplexity research pass + an adversarial review that verified every load-bearing claim against the codebase and current docs.

**Integration model: Bevy owns the iOS app** (reject the research's Swift-shell model). `main.rs` is a standard `DefaultPlugins` + `bevy_winit` app, and winit's UIKit backend runs the *whole* app on `aarch64-apple-ios`. Because `wc-core` is Bevy-native (63/72 files), Bevy-owns-the-app shares the entire existing app (HDR pipeline, egui chrome, Core/Sketches plugins); native capabilities (Vision, AVAudioSession, `isIdleTimerDisabled`, `thermalState`, document picker) are `objc2` leaf shims under the `platform/` convention. *Dropped:* the Swift-shell model; the "Bevy-free `waveconductor_core`" refactor (fantasy — 63/72 files are Bevy-coupled); AVAudioEngine (cpal has a real iOS CoreAudio backend — the fundsp+rtrb graph runs unchanged; only a ~20-line `AVAudioSession` shim is needed).

**Interaction on iPad:** touchless is primary here too — **Apple Vision hand-tracking is required, not optional** (the kiosk outputs to a projector; touch is the secondary mode). Hand-Z isn't required, so Vision-2D (no LiDAR fusion) is acceptable across the deck.

**#1 GPU-port risk:** the multi-pass HDR + gravity-post (framebuffer readback) + bloom + 6-pass blur stack at retina res — TBDR mobile GPUs punish full-screen framebuffer-readback passes. wgpu→Metal *compute* itself is fine on A12Z (WebGPU-only / compute-only is a *web-target* constraint, not a native-Metal one). Mitigated by `ios-dynamic-resolution` and dropping bloom/blur under `Hot`. Must be proven by the on-device soak.

### `ios-foundations`

`aarch64-apple-ios` (+ sim) targets; a macOS CI runner compiling iOS behind the lint gate; an Apple Developer account + provisioning/signing; gate `hand-tracking-gestures` (Leap) / `rfd` / dev-settings-panel *off* in the iOS profile. `cargo-mobile2` / `xcodebuild` wraps the Xcode project. **Exit:** `cargo build --target aarch64-apple-ios` green in CI.

### `ios-first-light`

winit UIKit app launches on-device, draws the Home picker + Line at native res, touch drives the attractor (touch first only because it's the easiest bring-up signal); FFI shims: `AVAudioSession` category/activation, `isIdleTimerDisabled`. **Exit:** Line interactive on-device, audio audible.

### `ios-thermal-soak`

`ThermalSource::OsThermalState` backend mapping `ProcessInfo.thermalState` → `ThermalTier` (via `thermal-seam-generalization`, bypassing the °C classifier — the OS provides hysteresis). Run `perf-soak-telemetry` **on the iPad**. **Exit (go/no-go):** an 8-hour on-device soak that either holds fanless thermals on the present-rate throttle, or produces the data justifying `ios-dynamic-resolution`. Depends on `thermal-seam-generalization`, `perf-soak-telemetry`.

### `ios-vision-hands`

*Required for the iPad kiosk (touchless primary).* An Apple Vision (`VNDetectHumanHandPoseRequest`) provider as a new `HandTrackingProvider` impl emitting the existing `HandTrackingFrame` shape (CF #76 reserves a second `ProviderId`). Caveats: Vision runs async, so the provider runs detection on its own queue and `poll()` drains a lock-free buffer (the WebSocket provider's pattern); Vision returns **2D landmarks, no Z** — acceptable per the interaction model (no sketch needs hand-Z). LiDAR depth fusion is a future enhancement, not in scope.

### `ios-dynamic-resolution`

*(Conditional on `ios-thermal-soak` evidence.)* The `dynamic-resolution` Image-target downscale for the Line 2D + gravity-post pass, tier-driven, with native-res UI composite. **Exit:** soak holds with UI staying crisp. Depends on `dynamic-resolution`.

### `ios-kiosk-hardening`

- **Crash recovery:** MDM / Apple Configurator **Single App Mode on a supervised device** (`com.apple.app.lock`) — Guided Access does *not* relaunch a crash or survive reboot; there is no iOS `Restart=always`. Mostly ops / provisioning.
- **Battery longevity:** a 2020 iPad pinned at ~100% under fanless GPU load 12 h/day risks swelling, and the user-settable 80% charge cap is iPad-2024+-only (the A12Z gets only automatic charge-management). Mitigate with ventilation, lower DRS to shed heat, an overnight duty-cycle, scheduled swelling inspection.
- **Permissions:** pre-grant camera/mic at setup so no system prompt strands the piece mid-install.

**Exit:** survives an unattended multi-day run + a forced crash.

---

## Phase 4 — New sketches & major features

Post-parity creative expansion.

### `face-body-tracking`

A new touchless interaction surface beyond hands — face and/or body pose tracking, via Apple APIs (Vision body pose / ARKit body tracking, TrueDepth face) on iPad, or MediaPipe for cross-platform / webcam. Slots into the same `HandTrackingProvider`-style provider abstraction (or a sibling pose provider). Opens new sketch interaction modes (lean / approach, face-driven, full-body). Down the road.

### `new-sketches`

Original post-v4 sketches and other major features. Design each with the universal audio-coupling pattern from day one (derive audio from CPU-side inputs, never GPU readback). Open-ended.

---

## Phase 5 — Web showcase

*Lowest priority.* The working v4 app already covers the web target, so the v5 web build is mainly a **portfolio showcase**, not the installation. WebGPU-only (no WebGL2 / CPU fallback; compute-shader particle path only).

Keeping web as a live target nonetheless **informs architecture along the way**: it's why frame-time-primary governance matters (no thermal sensor in the browser), why the settings-persistence layer already carries a `web-sys` localStorage path, and why the input layer keeps a `websocket` provider. Build the actual bundle last.

### `web-showcase`

The WASM/WebGPU portfolio build + bundle. Inherits the frame-time-primary governor as its only adaptive lever.

---

## Release gates (tag `v5.0.0`, merge `v5-alpha` → `main`)

Cross-cutting items that must be true before tagging `v5.0.0` and merging to `main`. Most map to slugs above; the rest:

- **Distribution** (spec §5.7) — macOS DMG, Windows portable exe, AppImage, web bundle, and (if iPad is taken) an `aarch64-apple-ios` `.ipa`. CI matrix + signing + notarization. Asset-path config for release bundles lands incrementally; iOS bundles assets inside the `.app`.
- **8-hour soak** (AGENTS.md) — required before every release tag (`perf-soak-telemetry` is the full-render version).
- **Perf audit harness** (spec §5.9) — `FrameTimeDiagnosticsPlugin` / `EntityCountDiagnosticsPlugin` / `SystemInformationDiagnosticsPlugin` → CSV (overlaps `perf-soak-telemetry` + `frametime-percentiles`). `bevy_framepace` spike — adopt if it improves thermal behaviour, skip if free-running already meets the bar.
- **v4 perf-mode shim** (spec §5.11) — a small IPC + start/stop bridge so v4 can stay on `main` until v5.0 is feature-complete.
- **Licenses surface** — port v4's `/licenses` route (the credits cell currently renders "Open Source Licenses" as plain text). Generate the dependency-license bundle (`cargo-about` / `cargo-bundle-licenses` in CI), ship it as an asset, wire an in-app modal through the overlay chrome. Reference `.worktrees/v4/src/routes/licensesPage/`.

---

## Reference: universal audio-coupling pattern

*Codified during Line's audio re-tune. Applies to every sketch.*

**Audio derives from CPU-side simulation *inputs*, never from GPU-side simulation *outputs*.**

Line's GPU-compute pipeline created a CPU↔GPU sync problem: the audio coupling needed per-frame particle statistics, but the authoritative state lived on the GPU. The fix replaced a parallel CPU physics mirror with smoothed CPU envelopes driven by `MouseAttractorState` events — audio reads attractor power directly (~1µs/frame vs ~50µs). v4's Cymatics already does this: its GPU compute is driven by CPU-side parameters and the audio reads those same parameters; the GPU is never read back.

Apply to future sketches:

- Identify the CPU-side inputs that drive the simulation (pointer/hand position, attractor power, time-since-event, mode/setting changes).
- Derive audio control signals from those inputs, not from per-particle / per-cell GPU-state reductions.
- Use smoothed envelopes (attack/release on input edges) for the right perceptual shape; tune constants against v4 perceptually; document as named consts.
- Approved deviation: audio won't be frame-by-frame mathematically equal to v4, but IS perceptually equivalent — document per `PARITY.md`.

Per sketch: Flame — shipped with envelope/DSP audio (no visitor stats; analytic `|dcX/dt|` + warp speed + camera distance drive the synth). Dots — keep particles CPU-side. Cymatics — copy v4 directly (CPU drives the GPU-compute inputs; audio reads them). Waves — audio is *input* (mic FFT), no coupling concern. The `AudioCommand::Add<Sketch>Synth` + per-param-over-a-lock-free-ring shape established for Line is the template.

---

## Shipped history

Immutable ledger of shipped work. Full detail in the per-plan docs under `docs/superpowers/plans/`.

| Plan | Topic | Tag |
| ---- | ----- | --- |
| 1 | Foundation (workspace, CI, lint gates) | `v5-foundation` |
| 2 | Lifecycle (state machine, leafwing keyboard actions) | `v5-lifecycle` |
| 3 | Input (mouse, touch, hand-tracking provider, pointer state) | `v5-input` |
| 4 | Audio scaffolding (cpal stream, ring buffers, default-silent DspHost) | `v5-audio` |
| 5 | Settings (Reflect-based, persistence, dev/user panels, derive macro) | `v5-settings` |
| 6 | Line skeleton + sketch scaffolding pattern | `v5-line` |
| 7 | Line simulation parity + idle veto hook | `v5-line-sim` |
| 7.5 | Test harness: synthetic input + shared `tests/common/` | `v5-test-harness` |
| 8 | Line rendering parity (gravity smear, star sprites, attractor rings) | `v5-line-render` |
| 9 | Line audio + reactivity coupling | `v5-line-audio` |
| 10 | Line polish + heatmap spawn + soak harness | — (parity gaps → Plan 11) |
| 11 | Line parity completion (rings, touch/hand activation, file picker, audio re-tune) | — (tag → `line-parity-signoff`) |
| 11.5 | Overlay UI parity (translucent buttons, settings chrome, nav, auto-fade, HDR pipeline) | — (tag → `line-parity-signoff`) |
| 11.6 | Hand-tracking `LeaprsProvider` + Leap manual verification + HandMesh | — (tag → `line-parity-signoff`) |
| 11.8 | Line screensaver / attract mode + adaptive thermal | — (carry-forwards → `screensaver-attract`) |

Notes preserved from the shipped narratives that bear on open work:

- **11.5 approved deviations** (record at `line-parity-signoff`): `panel_stroke` alpha 20→60, `button_stroke` 38→76, `panel_fill` 204→160 (browser backdrop-filter compositing); overlay buttons use `backdrop_blur_frame`; fade-overlay reload (v4 is instant); sheen 30° rotation on a horizontal strip; credits "Open Source Licenses" is plain text (no in-app page yet); egui has no letter-spacing knob; HDR + AgX + bloom (intensity 0.15, `Bloom::NATURAL`) pipeline matching v4's float-precision / browser-tonemap path.
- **11.5 not-shipped** (→ carry-forwards): fullscreen-toggle overlay button (keybinding exists), info/about button, dev-panel section grouping.
- **11.6 leaprs notes:** `leaprs 0.2.2`; `vendor/leapc/` soname version 6; `unsafe impl Send+Sync` on the connection; API-surface caveats — see CF #78–82.

The per-plan implementation docs (`docs/superpowers/plans/`) carry the full as-built detail for each line above.

---

## Convention

- Forward work is a **slug** (`kebab-case`); ordering / dependencies live in *Sequence & priorities*. Discrete shippables close with a commit + `v5-<slug>` tag.
- Detailed per-item plans live at `docs/superpowers/plans/YYYY-MM-DD-v5-<slug>.md`, written via the `superpowers:writing-plans` skill and executed via `superpowers:subagent-driven-development`.
- Each plan has a **Phase 0** that absorbs the current `next-plan-carry-forwards.md` items; new items found during review roll forward.
- Sketch-touching plans update / create `crates/wc-sketches/src/<sketch>/PARITY.md` at the sketch's parity target.
- Work lands on `v5-alpha`; `v5.0.0` tags the parity release and merges to `main`.
