# wc-capture: Visual-Debugging Scaffold — Design

**Goal:** Promote this sprint's ad-hoc visual-debugging techniques into a first-class, deterministic, agent-driven capture + regression scaffold for WaveConductor's rendered sketches.

**Status:** Design approved (scope + capture mechanism). Pending spec review → implementation plan.

**Date:** 2026-05-29

---

## Background

Debugging the Line hand-mesh render path this sprint relied on three improvised techniques that proved decisive but lived in throwaway `/tmp` scripts:

1. **Launch → screenshot-burst → per-frame metrics.** A script launched the app in a fixed scenario, captured frames, and printed a cheap per-frame metric (region mean RGB, per-row uniformity). The metric told the agent *which* frames to open and view — the agent applied the visual judgment.
2. **Render-stage isolation by elimination.** Temporary env-gated toggles (`force g_constant`, skip smear, disable bloom, disable overlay/composite, solid-color particles) bisected the pipeline and proved the compositor was not the dimmer.
3. **Deterministic scenario launch + clean-config isolation.** Env overrides for hand provider and start sketch, plus a clean (empty) config dir so stale on-disk settings could not confound diagnosis.

The improvised harness had one correctness flaw worth designing out: it captured at **wall-clock times after the window appeared**, so two runs whose windows appeared at different moments sampled different points on the gravity-smear triangle wave — producing a false "dimmer with a hand" reading that cost real investigation time.

## Goals / Non-goals

**Goals**
- Deterministic, reproducible frame capture keyed to sim state (not wall-clock).
- Single agent-first entrypoint (`cargo xtask capture`), `--json` output, self-documenting, documented in a `CLAUDE.md`.
- First-class render-stage isolation toggles (no edit-rebuild-revert).
- Regression detection against committed baselines, surfacing only changed frames.
- Zero added API spend: the operating agent is the visual judge (reads PNGs itself); the scaffold does only deterministic work. (Per the project's operator-in-the-loop rule.)
- Fully Rust: the app self-captures via Bevy's screenshot API; `xtask` does orchestration + image work via the `image` crate. No Python, no `screencapture`, no Quartz window-find.

**Non-goals**
- No LLM/vision API call for judgment — the agent judges by viewing PNGs.
- Not a pixel-perfect gate; tolerance-based diff + agent review.
- Not a replacement for unit/integration tests; this is for *visual* behavior the type system can't assert.
- Release builds are unaffected: capture + debug toggles compile out (`#[cfg(debug_assertions)]` or a `capture` feature).

## Architecture

```
cargo xtask capture <scenario> [--update-baselines] [--json] [--watch[=secs]] [overrides]
        |
        | resolves scenario -> env (provider, start-sketch, clean WAVECONDUCTOR_CONFIG_DIR,
        |                            WC_DEBUG_*, WC_CAPTURE=schedule+dir)
        v
  launches waveconductor (debug) ----> ① in-app capture system (wc_core::capture)
        |                                  - pins a fixed sim dt during capture
        |                                  - bevy screenshot at scheduled frames
        |                                  - writes target/capture/<scenario>/frame_NNNN.png + run.json
        |                                  - AppExit after the last frame
        |                              ② debug toggles (wc_core::debug::DebugToggles from WC_DEBUG_*)
        v
  app exits; xtask reads PNGs + run.json
        |
        | ③ metrics (image crate): region means, uniformity, frame-to-frame delta
        | ③ baseline diff vs tests/visual/baselines/<scenario>/ (tolerance)
        v
  report: human (default) + --json; lists changed/anomalous frames for the agent to open & judge
  exit code: 0 pass / non-zero regression (CI-gating)
```

Four isolated components, each with one responsibility:

### ① In-app capture system — `wc_core::capture`

- **Activation:** present only when `WC_CAPTURE` env is set; module gated `#[cfg(debug_assertions)]` (or a `capture` cargo feature). No-op / absent in release.
- **`WC_CAPTURE` format:** `key=value` pairs, `;`-separated. Required: `dir=<path>` (output dir), `frames=<n,n,...>` (sim-frame indices to capture). Optional: `dt=<secs>` (fixed timestep, default `1/60`), `settle=<n>` (frames to wait after assets-ready before frame 0, default a small constant). Example: `WC_CAPTURE="dir=target/capture/line-synthetic;frames=30,60,120,240"`.
- **Determinism:** while capturing, pin the virtual clock to a fixed `dt` so update *N* maps to sim time *N·dt*. Capture frame counting starts only once the sketch is entered **and** required assets are loaded (star sprite, AgX LUT), so frame 0 is the first fully-loaded sketch frame. (Mechanism — fixed virtual-time advance vs `Time<Fixed>` — decided in the plan; the contract is "frame N is reproducible.")
- **Capture:** at each scheduled frame, request a Bevy screenshot of the primary window's framebuffer → `<dir>/frame_NNNN.png` (zero-padded by frame index). Pixel-exact, no window chrome.
- **Sidecar:** write `<dir>/run.json` — scenario name, captured frame indices, `dt`, effective `LineSettings`/relevant settings, active `WC_DEBUG_*` toggles, app version/commit. Makes a capture self-describing and reproducible.
- **Exit:** after the last scheduled frame is written, send `AppExit`. The xtask also enforces a wall-clock timeout as a safety net.

### ② Render-stage debug toggles — `wc_core::debug::DebugToggles`

- A resource parsed once at startup from the `WC_DEBUG_*` env namespace; gated `#[cfg(debug_assertions)]`. Relevant systems/nodes read the resource instead of calling `std::env` directly (and instead of being patched by hand mid-debug).
- **Initial curated set** (promoted from this sprint):
  - `WC_DEBUG_FORCE_G=<f32>` — pin the Line gravity-smear `g_constant` (eliminates the triangle-wave phase variable).
  - `WC_DEBUG_DISABLE_SMEAR` — skip the gravity post-process node.
  - `WC_DEBUG_DISABLE_BLOOM` — zero/disable the main camera bloom.
  - `WC_DEBUG_DISABLE_BONE_COMPOSITE` — skip the bone-composite node.
  - `WC_DEBUG_DISABLE_BONE_CAMERA` — do not spawn the off-screen bone camera.
  - `WC_DEBUG_SOLID_PARTICLES=<rgba hex>` — render particles as a flat color (the "magenta" isolation trick), to separate particle geometry from texture/smear.
- **Render-world reach:** toggles consumed by render-graph nodes (smear, bone composite) are mirrored into the render world via the existing `ExtractResource` pattern; main-world toggles (bloom config, force-g, solid-particle material flag) are read in their owning systems.

### ③ `cargo xtask capture` — orchestration, metrics, regression

- **Scenarios:** named in a committed `tests/visual/scenarios.toml`: `name -> { sketch, provider, config (clean|path), debug = {…}, frames = [...] , dt? }`. `cargo xtask capture --list` prints them (self-documenting). Ad-hoc overrides via flags (e.g. `--debug FORCE_G=8000`). Baselines key off the scenario name.
- **Launch:** resolve scenario → env; `WAVECONDUCTOR_CONFIG_DIR` defaults to a fresh empty temp dir (clean settings) unless the scenario pins one. Run the debug binary; tee stdout+stderr to `<dir>/app.log`.
- **Metrics** (`image` crate over the PNGs): per-frame region means (full + center), global std / per-row uniformity, and frame-to-frame mean-abs-delta (frozen-vs-animated). Emitted to `<dir>/metrics.json`.
- **Baselines + diff:** committed PNGs under `tests/visual/baselines/<scenario>/frame_NNNN.png`. Diff = mean per-pixel absolute difference (and % pixels over a per-pixel threshold); a scenario passes if every frame is within tolerance. `--update-baselines` copies the current captures into the baseline dir.
- **Report:** default human-readable table; `--json` emits a machine shape (per-frame metrics + diff verdict + paths to current/baseline/anomalous frames). The report explicitly names which frames the agent should open and view.
- **Exit code:** `0` on pass (and for `--watch` / `--update-baselines`); non-zero when one or more frames regress beyond tolerance (CI-gating).

### ④ Live-demo — `cargo xtask capture <scenario> --watch[=secs]`

Launches the scenario for hands-on human inspection (no capture; runs the normal variable-dt clock), quitting after `secs` (default ~10). The mode used to show the working compositor.

## Agent workflow (no API spend)

1. Agent runs `cargo xtask capture line-synthetic --json`.
2. The xtask launches the deterministic scenario, the app self-captures, the xtask computes metrics + diffs baselines, and emits a report naming changed/anomalous frames.
3. The agent **Reads those PNGs** (its own image-reading — no API call) and applies visual judgment: pass, or diagnose further (e.g., re-run with `--debug DISABLE_BLOOM` to isolate).
4. In CI / non-interactive runs, the non-zero exit gates the build and changed frames are retained under `target/capture/` for an agent to review later (queue-for-operator).

A `CLAUDE.md` (harness/docs) documents: available scenarios, `cargo xtask capture` flags, the `--json` shape, the `WC_DEBUG_*` toggles, how to add a scenario, and how/when to update baselines.

## Cross-cutting concerns

- **GPU/driver nondeterminism:** float differences across GPUs make exact-match baselines brittle. Mitigation: tolerance-based diff (not exact); baselines are captured on, and intended for, the deployment-class machine; the agent reviews flagged frames rather than trusting a hard gate. Document that baselines are environment-sensitive.
- **Headless/CI rendering:** capture needs a real render surface. macOS dev has a display; CI/Linux would need an offscreen GPU path. Out of scope for v1 (capture is a dev/agent tool on the dev machine); note the constraint.
- **Determinism scope:** fixed-dt pins the *visual* sim (particles, smear, synthetic-hand sweep, `g_constant`). The audio thread is irrelevant to captured visuals.
- **Performance rules (AGENTS.md):** capture + toggles are debug-only and compiled out of release; no per-frame allocation added to release paths.

## Testing the scaffold itself

- Unit-test `WC_CAPTURE` / `WC_DEBUG_*` parsers (format edge cases).
- Unit-test the metric functions (mean, uniformity, frame-delta) on synthetic images.
- Unit-test the baseline diff (identical → 0; known delta → expected value; tolerance boundary).
- A smoke scenario captured + diffed against its own freshly-written baseline (round-trip) in an existing-display dev run; not added to headless CI.

## Open questions (resolve during planning)

1. Fixed-timestep mechanism in Bevy 0.18 (pinned `Time<Virtual>` advance vs `Time<Fixed>` + capture in `FixedUpdate`), and the cleanest asset-ready gate.
2. Cargo feature (`capture`) vs `#[cfg(debug_assertions)]` gating — feature is more explicit and lets release-profile captures exist if ever needed; `debug_assertions` is zero-config. Lean: a `capture` feature, off by default, enabled by the xtask's launch.
3. Baseline storage size/format — PNG at 1280×720 is small; confirm the storage approach (plain committed PNGs vs Git LFS) and which scenarios get baselines.
