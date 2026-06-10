# Line Screensaver / Attract Mode — Design Spec

**Date:** 2026-05-29
**Status:** Approved for autonomous implementation (owner away; proceed to a visually-verified screensaver/attract mode)
**Branch:** `rewrite/bevy`

---

## 1. Purpose

The screensaver/attract mode serves three goals, in priority order:

1. **Thermal stability for unattended multi-day installation.** The box (a NUC at a festival; dev on an M1 MacBook Pro) must shed heat during idle periods so it can run unattended for days without a technician babysitting it. This is the motivating problem of the whole v5 rewrite ("the v4 stack pins CPU during sketch idle periods enough to trigger thermal throttling").
2. **Attract mode ("tripper trap").** What's on screen during idle should be beautiful and hypnotic — it should *sell the actual experience* and beg passers-by to come play. The attract visual must be **visibly, unmistakably the real sketch**.
3. **On-screen instruction.** Teach visitors *how* to interact ("wave your hands over the head"). The sensor usually lives inside a head sculpture, occasionally something else (once a pie), so any textual instruction must be operator-customizable per install.

## 2. Decisions (from brainstorming + senior-engineer debate)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **Adaptive / thermal-aware tiers** (`Cool · Warm · Hot`) rather than a fixed cooldown or fixed-rich attract. | Most robust for unattended runs: stay rich when cool, ratchet down only when actually hot. |
| D2 | **Approach A — reuse the real Line pipeline, driven by synthetic attractors.** Not a separate renderer (B). | Guaranteed visual fidelity + faithful gesture teaching (real convergence) + instant-on (pipeline never torn down) + rides existing soak coverage. See §9. |
| D3 | **Composition = Autonomous Dreaming (subtle base) + Invitation Pulse (spine).** Presence-reactivity (#4) deferred; seam left open. | Owner's pick. The pulse doubles as the instruction (D5). |
| D4 | **Per-sketch attract registration via an `in_screensaver(AppState)` run-condition** (mirrors the existing idle-veto / `sketch_active` idiom), not a heavyweight registry. Only Line authored now. | Matches existing codebase idiom; minimal machinery; other sketches plug in later. |
| D5 | **Unified particle gesture = the instruction.** The Invitation Pulse's phantom hands perform a legible "hands over a central vessel anchor" gesture, rendered in the sketch's own particles. | Owner: "the attract mode sells the actual experience." Collapses tripper-trap + how-to into one animation. |
| D6 | **Caption text OFF by default.** Optional operator-set copy (headline + subline); renders only when non-empty. | Owner: "by default I just want to communicate visually." |
| D7 | **Hot tier = "resting ember"** via the per-tier **present-rate throttle** (~3 fps), **not** a frozen compute dispatch and **not** a pre-rendered video. | See the implementation addendum (§10): freezing the dispatch (`particle_count = 0`) black-screens because particle alpha only rises inside the compute shader, so "Low-Rate Ember" (full pipeline at ~3 fps) is the shipped Hot model — ~10× cooldown with no black-screen failure mode. A true warm-up-then-freeze is a deferred, soak-telemetry-gated escalation. |
| D8 | **Pause the Leap service during idle/screensaver** as a parallel thermal lever. | Research: the Ultraleap tracking service is a heavy constant CPU load (~44% of a core when tracking) — the likely M1-overheating culprit. Orthogonal to the renderer choice. |
| D9 | **Design-for-but-defer:** the shared `ThermalState` signal and the per-sketch hook are built minimal-but-general so (a) other sketches' attract visuals and (b) in-sketch *active* auto-adaptation can consume them later with no rework. | Owner flagged both for architectural planning, not for building now. |

## 3. Architecture — four seams

Three seams are built now (D2/D4); the fourth (Leap pause, D8) is a parallel, well-scoped addition. All are shaped to honor AGENTS.md (one concept per file, `///`/`//!` docs, no `unwrap`/`expect` in non-test code, no `as` numeric casts, platform code under `platform/`, zero systems when idle, no hot-path allocations, GPU resources despawned on the right transition).

### Seam 1 — `ThermalState` signal (foundation, `wc-core`)

A small `ThermalMonitorPlugin` maintaining `Res<ThermalState>`:

```
ThermalTier { Cool, Warm, Hot }
ThermalState { tier, last_temp_c: Option<f32>, source: ThermalSource }
ThermalSource { Sensor, GpuTimeProxy, Schedule }
```

- **Signal source priority (degradation chain):** real temperature sensor → GPU-time throttle-detection proxy → conservative time-based schedule (floor). The monitor tags each reading with its `ThermalSource` for the dev panel/logs.
- **Why not raw frame time:** the screensaver *deliberately* lowers fps, so wall-clock frame time is meaningless. Temperature (where available) is a *leading* indicator (drop to attract before the GPU throttles); the GPU-time proxy measures compute-time-per-fixed-workload against a cool baseline (hardware throttling = same work getting slower) and is a *lagging* fallback.
- **Libraries (to verify at implementation — D-verify):**
  - Linux (deployment NUC): **`sysinfo`** `Components` temperature API; world-readable sysfs, no root. (Intel iGPU exposes only CPU-package temp — acceptable; the SoC throttles together.)
  - macOS Apple Silicon (dev): **`macmon`** as a library (IOReport/IOKit, no sudo). `sysinfo` returns empty Components on Apple Silicon. `macmon` is a young *library* surface depending on a private Apple API → wrap behind our own trait, pin the version, treat any error as "no sensor → degrade."
  - No `nvml-wrapper` unless the final NUC ships NVIDIA.
- **Concurrency:** `macmon::get_metrics()` blocks for its sampling window → run sampling on a dedicated thread / task pool, publish via a lock-free channel; a tiny main-schedule system drains it and applies **asymmetric hysteresis** (enter Hot high, leave Hot lower) so the tier never flaps. Thresholds are placeholders to be tuned against the 8-hour soak on real hardware.
- **Generality (D9):** one resource anyone can read; only the screensaver consumes it now.

### Seam 2 — Screensaver framework (`wc-core`, promote `lifecycle/screensaver.rs`)

Today `screensaver.rs` is a placeholder (`show`/`hide` insert/remove a marker). It becomes the framework that:

- Provides the **`in_screensaver(AppState)` run-condition** (parallel to `sketch_active` in `sketch/scheduling.rs`) so each sketch gates its own performance systems on it.
- Owns a core **`ScreensaverSettings`** resource (User-category): `caption_headline: String`, `caption_subline: String` (both empty by default → no caption), plus room for later fields (e.g. a vessel-sprite path for the pie case). Persisted by the existing settings layer.
- Renders the **instruction overlay** (egui) — a lower-third caption that appears only when copy is set, fading in with the pulse. Pure opt-in (D6).
- **Throttles the present rate by tier** (Cool ≈ 24–30 fps, Warm lower, Hot ≈ 2–5 fps) via Bevy's winit update-mode / frame pacing (built-in `UpdateMode::Reactive { wait }` preferred over a new dependency — D-verify).
- Provides a **default fallback visual** when the active sketch registers no performer.

### Seam 3 — Line attract driver (`wc-sketches`, new `line/screensaver.rs`)

Systems gated on `in_screensaver(AppState::Line)`:

- **Choreography:** 1–2 slow wandering "dream" attractors (base) + the rhythmic **Invitation Pulse** — two phantom-hand attractors fade in above a central **vessel anchor**, ramp `power` to "grab," particles converge, hands lift, particles relax. This is both the tripper-trap and the gesture lesson (D5).
- **Writes the full `LineSimParams`** itself (the normal `update_sim_params` writer is gated on `Active` and does not run here). **Condition (A1):** refactor the param-baking core out of `update_sim_params` into a shared `bake_sim_params(...)` used by both the live and phantom writers so they cannot drift. Also drive `LinePostParams` (`g_constant`, `i_global_time`) so the gravity smear stays alive.
- **Scale by tier — Condition (A2):** allocate the particle storage buffer once at festival-max count (in `spawn_line`), then set `LineSimParams.particle_count` per tier. `prepare_bind_group` recomputes `dispatch_size = particle_count.div_ceil(64)` each frame, so lowering the count lowers the dispatch with **no reallocation**. A single integer write per tier transition.
- **Hot tier freeze (D7):** add a thermal-tier predicate to `prepare_bind_group`'s `run_if` so it stops producing a fresh `LineComputeBindGroup` at Hot. `LineComputeNode::run` then early-returns (no bind group) and dispatches zero workgroups — particles freeze, still drawn and dimly bloomed. Genuine compute cooldown.

### Seam 4 — Leap idle-throttle (parallel, `wc-core` `input/providers/leap_native.rs`) — D8

- Arm `PolicyFlags::ALLOW_PAUSE_RESUME` alongside the existing deferred `BACKGROUND_FRAMES` policy (same async-handshake retry block). `leaprs` 0.2.2 already exposes `Connection::set_pause` and the policy flag — **no FFI/fork needed** (verified against the vendored `LeapC.h` and the crate source).
- Add a typed `set_paused(&mut self, bool)` on `LeaprsProvider` (mirrors `apply_background_policy`), and a `PreUpdate` system that pauses on entering `Idle`/`Screensaver` and resumes on returning to `Active`.
- **Wake:** the safe default keeps wake instant by resuming on first interaction. Because a *paused* service emits no frames, pausing trades instant hand-detection for CPU savings; mitigations (periodic "blink" un-pause sampling; or an external proximity sensor) are documented for the owner to choose. Default ship: pause on `Screensaver` only (not mere `Idle`), resume on any interaction event; revisit wake strategy after hardware measurement.
- **Honest caveat (hardware-verify):** software pause reliably cuts *host CPU* but its effect on *controller heat* (IR-LED power-down) is inconsistent across SDK builds. The only guaranteed device-heat fix is cutting USB power. This seam targets the host-CPU/M1 win; controller-heat mitigation is flagged for the owner's hardware test, not promised here.

## 4. Composition detail

```
RESTING DREAM (base)         particles drift under 1–2 slow wandering attractors; never identical
        │  every ~N s
        ▼
INVITATION PULSE (spine)     2 phantom hands fade in over the central vessel anchor,
   = THE INSTRUCTION         "grab" (power ramps), particles surge & converge,
                             hands lift, particles relax back into the dream
        │
        ▼
CAPTION (optional, off)      lower-third operator copy, fades with the pulse — only if set
```

Thermal tiers dial the *same* performance:

```
COOL  full attract particle budget · ~24–30 fps · full choreography
WARM  reduced particle_count (smaller dispatch) · lower fps · same choreography
HOT   dispatch STOPPED · particles frozen ("ember") · ~2–5 fps present · bloom breathing only
      ◀── climbs back up with hysteresis as it cools
```

## 5. Customization

- **Now:** `ScreensaverSettings.caption_headline` / `.caption_subline` (User panel; empty = hidden).
- **Later (designed-for):** a vessel-sprite `FilePath` setting so the central anchor + gesture can read as the actual vessel (head, pie, …); per-sketch caption overrides.

## 6. Non-goals / deferred

- Presence-reactive layer (#4) — both the free "hand-enters-volume" version and the IR-image near-field detector. Seam left open.
- Other sketches' attract visuals (Flame/Dots/Cymatics/Waves) — framework supports them; not authored now.
- In-sketch *active* auto-adaptation (live sketches self-throttling during play) — reads the same `ThermalState` later.
- Real OS temperature on every platform (Windows is best-effort; Apple-Silicon via private API).
- Hardware USB-power-cut for guaranteed controller-heat reduction.
- Pre-rendered video fallback (replaced by resting ember, D7).

## 7. Risks & open questions

| Risk | Mitigation |
|------|------------|
| **Throttled low-fps N-body may stutter** (B's strongest counter to A). | The decisive open question — *settle it with `cargo xtask capture` at each tier* (§9). Line's heavy inertial drag + temporal gravity-smear should mask low fps (long-exposure look); if captures prove otherwise, fall back to a B-style cheap field for the rest tiers only (hybrid). |
| `macmon` private Apple API / young library. | Wrap behind our trait; pin version; any error → degrade to GPU-time proxy → schedule. Dev-platform only. |
| Two writers of `LineSimParams` drift. | Condition A1: shared `bake_sim_params`. |
| Tier transitions reallocate the buffer (hot-path alloc / hitch). | Condition A2: buffer at festival-max, scale dispatch size only. |
| Full pipeline resident + running for many never-exited hours (the soak exercises spawn/despawn churn, not indefinite no-churn residency). | Add a `DefaultPlugins`, never-exited soak slice before the pi-party tag. |
| Leap pause doesn't cool the controller. | Targets host CPU; controller heat flagged for hardware test + optional USB-power-cut. |
| Leap pause delays hand-detection wake. | Default: pause only in `Screensaver`, resume on interaction; revisit blink/external-sensor after measurement. |

## 8. Verification plan (visual-first, no LLM API spend)

- **Visual capture (primary):** add a `line-screensaver` scenario (drives `SketchActivity::Screensaver`) to `tests/visual/scenarios.toml`; capture Cool/Warm/Hot tiers in separate runs (force the tier via a capture override). The operating agent reviews the PNGs directly — confirm: (a) visibly Line, (b) the gesture reads as "hands over the vessel," (c) throttled tiers still look intentional/beautiful, (d) Hot ember is calm-but-alive.
- **Unit tests:** `ThermalTier` classification + hysteresis (no flap at boundaries); choreography math (phantom-hand paths, pulse timing); `in_screensaver` run-condition.
- **Integration tests:** headless lifecycle — `Active → Idle → Screensaver` enters the performer; interaction returns to `Active`; screensaver systems run zero work outside `Screensaver` (schedule inspection via `bevy_mod_debugdump`).
- **Soak:** extend the 8-hour soak with a never-exited screensaver slice; log `ThermalState.last_temp_c` to set real thresholds.
- **Leap pause:** flagged for owner hardware verification (IR-viewer LED check; Activity Monitor CPU delta streaming vs paused; resume latency).

## 9. Why A over B (debate record)

Two senior engineers argued each side against the owner's benchmarks (beat v4's video-decode GPU load; no 12h+ choke; throttling must still look good; deep rest + instant-on; account for the Leap baseline). Both converged toward A-as-spine:

- **Load-bearing codebase facts:** the compute + post-process nodes are render-graph nodes gated only on *resource existence*, never on `SketchActivity`; only the param *writer* gates on `Active`. GPU resources despawn on `OnExit(AppState::Line)` — and Idle/Screensaver are *sub-states of Line* — so the pipeline stays resident through the screensaver. Attractors are already a clean `[Attractor; 8]` data interface (`power == 0` = inactive). A slots a new param *producer* into an existing seam.
- **A wins decisively on instant-on (no respawn cut) and long-uptime (rides existing soak), and wins at Hot (frozen dispatch = near-zero GPU, which a video loop can't match).**
- **B's real strengths:** lower floor in the *animated* tiers and "designed-for-low-fps looks intentional." B's own bottom line concedes A wins the *teaching* requirement and proposes a hybrid (cheap field for rest tiers, real particles for the active-teaching tier) — which is precisely our measured fallback if capture shows throttled A looks bad.
- **The largest thermal lever is the Leap service (D8), orthogonal to A/B.** So choosing A costs little thermally while keeping fidelity, teaching, and instant-on.

A is adopted with the three hard conditions A1/A2/A3 (shared bake fn; scale dispatch not realloc; prove throttled beauty via capture) folded into Seam 3 and §8.

---

## 10. Implementation addendum (2026-05-30, as-built + verified)

The feature was implemented across all four seams and then independently
verified (build, 301 workspace tests, clippy 0/0 incl. the `hand-tracking-gestures`
Leap path, and direct review of the capture PNGs). Two deviations from the
original spec were made during verification, each driven by a confirmed defect or
a verified third-party-library fact:

### 10.1 Hot tier: "Low-Rate Ember", not a frozen dispatch (revises D7 / Seam 3)

**Defect found:** the first implementation realized Hot by setting
`LineSimParams.particle_count = 0` (zero compute workgroups). Particles are
CPU-seeded with `alpha = 0` (`spawn.rs`) and alpha **only** rises inside the
compute shader (`simulate.wgsl`), so a never-dispatched buffer stays fully
transparent — a **black screen** (confirmed: Hot capture frames were `full_mean
0,0,0`). A black attract screen is the worst outcome for an unattended kiosk.

**Resolution (two senior-engineer debate, converged):** drop the dispatch freeze.
Hot is realized purely by the per-tier **present-rate throttle** that already
exists (Seam 2: Cool ≈ 30 fps, Warm ≈ 15 fps, Hot ≈ 3 fps via winit
`UpdateMode::Reactive`). At 3 fps the full pipeline runs ~1/10th as often as Cool
— a large, real cooldown — while alpha fades in normally, so Hot is a calm,
slowly-breathing ember, never black. This also deleted the `FullDispatchCount` /
`apply_thermal_tier` / `restore_full_dispatch` / `tier_dispatch_override`
machinery, making the Line attract driver thermal-tier-agnostic (cooldown is the
framework's job). Condition **A2 is therefore moot** (no per-tier dispatch
scaling). A true dispatch freeze remains a **deferred escalation**, and only as a
*warm-up-then-freeze* (run the dispatch until alpha saturates, then latch off),
gated on 8-hour-soak telemetry showing 3 fps + the Leap idle-pause (D8) still runs
the NUC too hot.

**Capture consequence:** because tier now changes only the present *rate* (which
the capture harness deliberately ignores — it pins its own virtual clock) and the
choreography is tier-agnostic, all tiers produce identical captured frames. The
three per-tier scenarios (`line-screensaver-{cool,warm,hot}`) and their baselines
(including the falsely-"verified" black Hot baseline) were collapsed to a single
`line-screensaver` scenario. Hot's cooldown is verified by soak telemetry + a
frame-timing assertion, not a baseline PNG.

### 10.2 Thermal sensing: zero-dependency Linux sysfs reader (revises Seam 1)

**Fact verified:** latest `sysinfo` (0.39, MSRV 1.95) genuinely does not build on
the pinned rustc 1.89 — but `sysinfo 0.33.1` does, and `macmon` (macOS) needs
1.95. So the initial "stub all sensing to a no-op on every platform" was wrong
(it left the headline adaptive-cooling feature inert at runtime everywhere).

**Resolution (library survey, MSRVs probed empirically on the real 1.89
toolchain):** the deployment target is a **Linux** NUC, where the kernel already
exposes temperatures at `/sys/class/hwmon/*/temp*_input` and
`/sys/class/thermal/thermal_zone*/temp` (world-readable millidegrees). A
**zero-dependency `std::fs` reader** (`platform/native.rs` `SysfsThermalSensor`,
gated on the `thermal-sensor` feature, default-on for the deployment binary) is
the most robust option for an unattended multi-day appliance: no supply chain, no
MSRV pin to babysit, no build-time native dep on a fresh NUC. It prefers the
`coretemp`/`k10temp` hwmon chips by name (the real CPU die sensor) and falls back
to CPU-ish thermal zones; verified to compile + lint clean for
`x86_64-unknown-linux-gnu` under rustc 1.89. `sysinfo` was removed entirely
(survey ranked it below pure-std; `lm-sensors` rejected for its libsensors +
bindgen build-time deps). macOS Apple-Silicon stays on the Cool/Schedule
no-sensor fallback (the `macmon` path is preserved behind the dormant,
`compile_error!`-guarded `thermal-sensor-macos` feature for a future toolchain
bump). The hysteresis classifier, sampler thread, and degradation chain from the
original Seam 1 are unchanged.

### 10.3 Verified status

Green: `cargo build -p waveconductor`; `cargo test --workspace` (301 passed, 0
failed); `cargo clippy --workspace --all-targets` and
`cargo clippy -p wc-core --features hand-tracking-gestures` (0 warnings, 0
errors); `native.rs` Linux cross-check (compile + clippy clean on 1.89); capture
PNGs reviewed (resting dream → grab → release arc, visibly Line, no black frame).

Still deferred (unchanged from §6, plus): the warm-up-then-freeze Hot escalation;
real macOS sensing (toolchain-gated); presence-reactivity; other sketches'
attract visuals; the 8-hour never-exited soak slice to tune the thermal
thresholds; hardware verification of the Leap pause's controller-heat effect.

### 10.4 Screensaver compute/thermal refinement (2026-06-10, post-MediaPipe)

After the MediaPipe merge (4 Hz idle inference throttle, `b3d6589a`) and the
wandering-pulses choreography (`3ee38e93`), the Seam 2 present-rate throttle was
audited for the screensaver compute/thermal budget. Findings and changes:

- **Restore defect fixed (real bug).** `restore_present_rate` hard-coded
  `Continuous` for both modes on exit. The app runs `WinitSettings::default()`
  (= `game()`): focused `Continuous`, unfocused `reactive_low_power(1/60)` — so
  one screensaver cycle silently upgraded an *unfocused* window to an uncapped
  burn. Entry now snapshots both modes into a `SavedPresentMode` resource
  (`OnEnter`) and any exit restores them exactly (`OnExit`), falling back to
  `WinitSettings::default()` if the snapshot is ever absent. Regression-tested
  end-to-end in `tests/screensaver.rs`
  (`present_rate_throttles_in_screensaver_and_restores_prior_modes`).
- **Cap named and derived.** The temperature-independent screensaver cap is now
  the documented `SCREENSAVER_FPS = 30.0` const; the Cool-tier wait derives from
  it (`1/30 s`). Arithmetic: against an uncapped/ProMotion 60–120 Hz display the
  cap cuts sustained render + particle compute + smear energy by ≥ 50% (the
  reactive loop gates the whole schedule, render world included). Warm/Hot tier
  waits compose *below* the cap, unchanged.
- **Hand-wake chain documented (MediaPipe).** Camera frames are not winit
  events; wake is polled, and nothing wakes the loop early, so each step costs
  a full reactive tick: 4 Hz idle inference emits a hand-bearing frame → tick
  N's `poll_all_providers` (PreUpdate) drains it and `reset_on_interaction` /
  `advance_activity` write `NextState` in tick N's Update — but
  `StateTransition` runs *before* Update in the `Main` schedule, so the
  `Active` flip and the snapshot restore land in tick N+1's `StateTransition`,
  a second full wait later. The throttle therefore adds ≤ 2 reactive ticks:
  ~66 ms at the 30 fps cap (≈ 366 ms total against the documented ~300 ms
  worst-case wake); Hot adds ≤ 666 ms (≈ 0.97 s worst case, accepted in a
  thermal emergency). During the screensaver the *unfocused* mode is set to the
  same reactive mode — intentionally more device-event-responsive than the
  `game()` unfocused baseline (`react_to_device_events: true` vs `false`) so a
  passer-by can wake an unfocused window.
- **No additional smear/compute gating.** At 30 fps the dispatch + smear already
  run at half rate via the present gate; a true dispatch freeze remains the
  deferred warm-up-then-freeze escalation (§10.1), still soak-gated.
