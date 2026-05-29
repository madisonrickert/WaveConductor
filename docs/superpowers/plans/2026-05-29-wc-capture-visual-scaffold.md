# wc-capture: Visual-Debugging Scaffold — Implementation Plan

**Spec:** `docs/superpowers/specs/2026-05-29-wc-capture-visual-scaffold-design.md` (approved)
**Date:** 2026-05-29
**Branch:** `rewrite/bevy`

---

## Goal

Promote this sprint's ad-hoc visual-debugging techniques into a first-class, deterministic, agent-driven capture + regression scaffold for WaveConductor's rendered sketches. Four isolated components:

1. **`wc_core::capture`** — in-app deterministic frame-capture system. Parses `WC_CAPTURE` once at startup; pins a fixed virtual-time `dt`; waits for assets-ready + a settle window; requests Bevy screenshots at scheduled sim-frame indices; writes `frame_NNNN.png` + `run.json`; sends `AppExit` after the last frame.
2. **`wc_core::debug`** — `DebugToggles` resource parsed once from the `WC_DEBUG_*` env namespace; mirrored into the render world via `ExtractResource` where render-graph nodes need it.
3. **`xtask capture`** — orchestration (scenario → env → launch debug binary), metrics (`image` crate), baseline diff, human + `--json` report, `--watch`, `--list`. Independent of wc-core/wc-sketches (shells out).
4. **Render-toggle wiring** — wc-sketches + main-app honour `DebugToggles` (force-g, disable smear/bloom/bone-composite/bone-camera, solid particles), and `LineBoneCompositePlugin` is hoisted into `LinePlugin::build` so its toggle gates at the LinePlugin level.

## Architecture

```
cargo xtask capture <scenario> [--update-baselines] [--json] [--watch[=secs]] [--list] [--debug KEY=VAL ...]
   |  resolve scenario from tests/visual/scenarios.toml -> env
   |    (WAVECONDUCTOR_HAND_PROVIDER, WAVECONDUCTOR_START_SKETCH,
   |     WAVECONDUCTOR_CONFIG_DIR=fresh-temp, WC_DEBUG_*, WC_CAPTURE="dir=...;frames=...")
   v
   launch `cargo run -p waveconductor` (DEBUG); tee stdout+stderr -> <dir>/app.log
   |
   |  in-app (debug-only, runtime-activated):
   |   (1) wc_core::capture  — pin Time<Virtual> dt; gate frame 0 on assets-ready + settle;
   |                           Screenshot at scheduled frames -> <dir>/frame_NNNN.png;
   |                           write <dir>/run.json; AppExit after last frame
   |   (2) wc_core::debug    — DebugToggles inserted ONLY when a WC_DEBUG_* var is present;
   |                           ExtractResource mirrors render-world toggles
   v
   app exits; xtask reads PNGs + run.json
   |   (3) metrics (image crate)  -> <dir>/metrics.json  (region means, uniformity, frame-delta)
   |   (3) baseline diff vs tests/visual/baselines/<scenario>/  (mean abs diff + % over threshold)
   v
   report: human table (default) | --json (per-frame metrics + diff verdict + paths + frames-to-open)
   exit 0 pass / nonzero regression
```

## Tech Stack

- **App side (in `wc-core`, `wc-sketches`, `waveconductor`):** Bevy 0.18.1. Screenshot via `bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, Captured, save_to_disk}`. Determinism via `bevy::time::TimeUpdateStrategy::ManualDuration(dt)`. Render-world toggles via `bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin}` (same pattern as `LinePostParams` / `HandMeshTarget`). Exit via `bevy::app::AppExit` written with `MessageWriter<AppExit>`.
- **xtask side:** `clap` (derive), `image` 0.25, `serde` + `serde_json`, `toml` — all already in `[workspace.dependencies]`. xtask shells out to `cargo run -p waveconductor`; it does NOT depend on wc-core/wc-sketches.
- **Baselines:** plain committed PNGs under `tests/visual/baselines/<scenario>/frame_NNNN.png`. No Git LFS.

### Gating decision (LOCKED — Option A hybrid)

- **Compile-gate** the capture module, the debug module, and ALL their plugin/system registration with `#[cfg(debug_assertions)]`, mirroring the existing const-level idiom in `crates/waveconductor/src/main.rs` (~lines 28–34, 61). Release builds compile out the whole scaffold.
- **Runtime-activate** via env: the capture system early-returns when `WC_CAPTURE` is unset; `DebugToggles` is *inserted as a resource* only when at least one `WC_DEBUG_*` var is present, so a normal `cargo run -p waveconductor` (debug) carries essentially nothing — no resource, no per-frame work beyond a cheap `Option<Res<_>>` miss.
- **Guard comment** at root `Cargo.toml` `[profile.release]` AND in the capture module: this scaffold relies on `debug-assertions = false` in release; never enable assertions in the release/soak profiles or the capture/debug systems will compile back in.

### Env parse-once rule (LOCKED)

Both `WC_CAPTURE` and `WC_DEBUG_*` are parsed exactly once, in a `Startup` system (or a plugin-`build` read), into resources. No per-frame `std::env` read. The per-frame capture system reads only the pre-parsed `CaptureConfig` resource.

---

## File Structure

### Created

| Path | Responsibility |
|------|----------------|
| `crates/wc-core/src/debug/mod.rs` | Module root: `DebugPlugin`, `DebugToggles` resource (`ExtractResource`), `WC_DEBUG_*` parse-once, `SolidParticleColor` parsing. `#[cfg(debug_assertions)]`-gated. |
| `crates/wc-core/src/capture/mod.rs` | Module root: `CapturePlugin`, re-exports config + system. `#[cfg(debug_assertions)]`-gated. Guard comment re: release debug-assertions. |
| `crates/wc-core/src/capture/config.rs` | `CaptureConfig` resource + `parse_wc_capture(&str)` (the `dir=…;frames=…;dt=…;settle=…` parser) + unit tests. |
| `crates/wc-core/src/capture/system.rs` | `CaptureState` (`Local`-like resource: phase, frame counter), the assets-ready + settle gate, `ManualDuration` pin, screenshot dispatch, `run.json` sidecar, `AppExit`. |
| `xtask/src/capture.rs` | `cargo xtask capture` subcommand: `Args`, scenario load, launch, metrics, diff, report, `--list`/`--watch`/`--update-baselines`. |
| `xtask/src/capture/scenarios.rs` | `Scenario` + `Scenarios` (serde) loader for `tests/visual/scenarios.toml`. |
| `xtask/src/capture/metrics.rs` | Pure metric fns (region mean, per-row uniformity/std, frame-to-frame mean-abs-delta) + `metrics.json` shape + unit tests. |
| `xtask/src/capture/diff.rs` | Baseline diff (mean per-pixel abs diff, % pixels over threshold, tolerance verdict) + unit tests. |
| `tests/visual/scenarios.toml` | Committed scenario table (`line-synthetic`, `line-synthetic-no-bloom`). |
| `tests/visual/baselines/line-synthetic/.gitkeep` | Placeholder; real `frame_NNNN.png` baselines land via `--update-baselines` in the smoke task. |
| `tests/visual/CLAUDE.md` | Harness docs: scenarios, flags, `--json` shape, `WC_DEBUG_*` toggles, add-a-scenario, update-baselines. |

### Modified

| Path | Change |
|------|--------|
| `crates/wc-core/src/lib.rs` | Declare `#[cfg(debug_assertions)] pub mod capture;` + `pub mod debug;`; register `CapturePlugin` + `DebugPlugin` in `CorePlugin::build` after `SettingsPlugin`, both `#[cfg(debug_assertions)]`. |
| `crates/wc-sketches/src/line/mod.rs` | Hoist `LineBoneCompositePlugin` registration into `LinePlugin::build` (gated on `!DebugToggles.disable_bone_composite`); gate `LinePostProcessPlugin` on `!disable_smear`. |
| `crates/wc-sketches/src/line/hand_mesh.rs` | Remove `LineBoneCompositePlugin` from `LineHandMeshPlugin::build` (now owned by `LinePlugin`); gate `spawn_hand_mesh_camera` `OnEnter` on `!disable_bone_camera`. |
| `crates/wc-sketches/src/line/audio_coupling.rs` | In `drive_audio_and_shader`, honour `DebugToggles.force_g` (pin `g_constant`) when present. |
| `crates/wc-sketches/src/line/material.rs` | Add `solid_color: Vec4` `#[uniform(3)]` to `LineMaterial`; default transparent (alpha 0 = "off"). |
| `crates/wc-sketches/src/line/systems/spawn.rs` | Seed `LineMaterial.solid_color` from `DebugToggles.solid_particles` at spawn. |
| `assets/shaders/line/render.wgsl` | Fragment: when `solid_color.a > 0.0`, return the flat colour instead of the star texel. |
| `crates/waveconductor/src/main.rs` | `#[cfg(debug_assertions)]` system that zeroes/restores `Bloom.intensity` from `DebugToggles.disable_bloom`. |
| `xtask/src/main.rs` | Add `Capture(capture::Args)` to `Command` enum + match arm; `mod capture;`. |
| `xtask/src/manifest.rs` | Add `capture` entry to `SUBCOMMANDS` (kept in sync with the `Command` enum). |
| `xtask/Cargo.toml` | Add `image`, `serde`, `serde_json`, `toml` (all `{ workspace = true }`). |
| `Cargo.toml` | Guard comment on `[profile.release]` re: never set `debug-assertions = true`. |

---

# Tasks

Order: wc-core foundation (capture config parser → capture system → debug toggles → CorePlugin registration) → xtask (independent: deps → scenarios → metrics → diff → subcommand → main/manifest sync) → wc-sketches/main-app render-toggle wiring (incl. the `LineBoneCompositePlugin` fix) → docs → final real smoke-capture verification.

Run all `cargo` commands from the workspace root `/Users/madison/Developer/WaveConductor`.

---

## Task 1 — `WC_CAPTURE` parser + `CaptureConfig`

**Files:**
- Create `crates/wc-core/src/capture/config.rs`

**Failing test** (footer of `config.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_required_fields_with_defaults() {
        let cfg = parse_wc_capture("dir=target/capture/line;frames=30,60,120").unwrap();
        assert_eq!(cfg.dir, std::path::PathBuf::from("target/capture/line"));
        assert_eq!(cfg.frames, vec![30, 60, 120]);
        assert_eq!(cfg.dt, Duration::from_nanos(16_666_667)); // ~1/60
        assert_eq!(cfg.settle, 2);
    }

    #[test]
    fn parses_optional_dt_and_settle() {
        let cfg = parse_wc_capture("dir=out;frames=1;dt=0.05;settle=5").unwrap();
        assert_eq!(cfg.dt, Duration::from_secs_f64(0.05));
        assert_eq!(cfg.settle, 5);
    }

    #[test]
    fn frames_are_sorted_and_deduped() {
        let cfg = parse_wc_capture("dir=out;frames=120,30,60,30").unwrap();
        assert_eq!(cfg.frames, vec![30, 60, 120]);
    }

    #[test]
    fn missing_dir_is_error() {
        assert!(parse_wc_capture("frames=1,2").is_err());
    }

    #[test]
    fn missing_frames_is_error() {
        assert!(parse_wc_capture("dir=out").is_err());
    }

    #[test]
    fn empty_frames_is_error() {
        assert!(parse_wc_capture("dir=out;frames=").is_err());
    }
}
```

**Run-to-fail:** `cargo test -p wc-core --lib capture::config`
**Expected failure:** compile error — `parse_wc_capture` / `CaptureConfig` do not exist (cannot find function/type).

**Minimal implementation** (full file `crates/wc-core/src/capture/config.rs`):

```rust
//! `WC_CAPTURE` env parsing and the parsed [`CaptureConfig`] resource.
//!
//! The capture system reads only this pre-parsed resource — it never touches
//! `std::env` per frame (project rule: parse env once at startup).
//!
//! Format (`;`-separated `key=value`):
//! `dir=<path>;frames=<n,n,...>[;dt=<secs>][;settle=<n>]`
//! - `dir`: output directory for `frame_NNNN.png` + `run.json`.
//! - `frames`: sim-frame indices to screenshot (frame 0 = first fully-loaded
//!   sketch frame, after assets-ready + settle).
//! - `dt`: fixed virtual-time delta in seconds (default `1/60`).
//! - `settle`: frames to wait after assets-ready before frame 0 (default `2`).

use std::path::PathBuf;
use std::time::Duration;

use bevy::prelude::Resource;

/// Parsed `WC_CAPTURE` schedule + output target. Inserted once at startup;
/// read each frame by the capture system. Absent when `WC_CAPTURE` is unset.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Output directory for `frame_NNNN.png` and `run.json`.
    pub dir: PathBuf,
    /// Sim-frame indices to screenshot, ascending and deduplicated.
    pub frames: Vec<u32>,
    /// Fixed virtual-time delta pinned during capture.
    pub dt: Duration,
    /// Frames to wait after assets-ready before counting frame 0.
    pub settle: u32,
}

/// Default fixed timestep: 1/60 s, expressed in whole nanoseconds so the value
/// is exact and equality-comparable in tests.
const DEFAULT_DT: Duration = Duration::from_nanos(16_666_667);

/// Default settle window: a small constant number of frames after assets-ready.
const DEFAULT_SETTLE: u32 = 2;

/// Parse a `WC_CAPTURE` value into a [`CaptureConfig`].
///
/// # Errors
///
/// Returns a human-readable `String` when `dir` or `frames` is missing, when
/// `frames` is empty, or when a numeric field fails to parse.
pub fn parse_wc_capture(raw: &str) -> Result<CaptureConfig, String> {
    let mut dir: Option<PathBuf> = None;
    let mut frames: Option<Vec<u32>> = None;
    let mut dt = DEFAULT_DT;
    let mut settle = DEFAULT_SETTLE;

    for pair in raw.split(';').filter(|s| !s.trim().is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("WC_CAPTURE: malformed pair (no '='): {pair:?}"))?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "dir" => dir = Some(PathBuf::from(value)),
            "frames" => {
                let mut parsed: Vec<u32> = value
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|n| {
                        n.trim()
                            .parse::<u32>()
                            .map_err(|e| format!("WC_CAPTURE: bad frame index {n:?}: {e}"))
                    })
                    .collect::<Result<_, _>>()?;
                parsed.sort_unstable();
                parsed.dedup();
                frames = Some(parsed);
            }
            "dt" => {
                let secs = value
                    .parse::<f64>()
                    .map_err(|e| format!("WC_CAPTURE: bad dt {value:?}: {e}"))?;
                dt = Duration::from_secs_f64(secs);
            }
            "settle" => {
                settle = value
                    .parse::<u32>()
                    .map_err(|e| format!("WC_CAPTURE: bad settle {value:?}: {e}"))?;
            }
            other => return Err(format!("WC_CAPTURE: unknown key {other:?}")),
        }
    }

    let dir = dir.ok_or_else(|| "WC_CAPTURE: missing required key 'dir'".to_string())?;
    let frames = frames.ok_or_else(|| "WC_CAPTURE: missing required key 'frames'".to_string())?;
    if frames.is_empty() {
        return Err("WC_CAPTURE: 'frames' must list at least one index".to_string());
    }

    Ok(CaptureConfig {
        dir,
        frames,
        dt,
        settle,
    })
}
```

**Run-to-pass:** `cargo test -p wc-core --lib capture::config`
**Expected pass:** 6 tests pass. (The module is not yet declared in `lib.rs`; declare it in the same edit so the file compiles — see the lib.rs change shipped in Task 4. Until then, add a temporary `#[cfg(debug_assertions)] mod capture { pub mod config; }` at the bottom of `lib.rs`. Task 4 replaces it with the real module tree. If you prefer, do Task 4's `lib.rs` module declaration now and skip the temporary.)

**Commit (operator runs later):**
`git add crates/wc-core/src/capture/config.rs crates/wc-core/src/lib.rs && git commit -m "wc-core/capture: WC_CAPTURE parser + CaptureConfig"`

---

## Task 2 — Capture system (assets-ready + settle gate, ManualDuration, screenshot, run.json, AppExit)

**Files:**
- Create `crates/wc-core/src/capture/system.rs`

**Failing test** (footer of `system.rs`) — exercises the deterministic *gate state machine* in a headless `MinimalPlugins` app (no real GPU; we assert the phase transitions and that `ManualDuration` is installed, not pixels):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::config::CaptureConfig;
    use bevy::time::TimeUpdateStrategy;
    use std::path::PathBuf;
    use std::time::Duration;

    fn cfg() -> CaptureConfig {
        CaptureConfig {
            dir: PathBuf::from("target/capture/__test"),
            frames: vec![0, 2],
            dt: Duration::from_millis(10),
            settle: 1,
        }
    }

    #[test]
    fn installs_manual_duration_when_capturing() {
        let mut app = bevy::app::App::new();
        app.add_plugins(bevy::time::TimePlugin);
        app.insert_resource(cfg());
        app.init_resource::<CaptureState>();
        // Force the gate to treat assets as ready (headless has no sketch).
        app.world_mut().resource_mut::<CaptureState>().assets_ready = true;
        app.add_systems(bevy::app::Update, pin_capture_timestep);
        app.update();
        let strat = app.world().resource::<TimeUpdateStrategy>();
        assert!(matches!(strat, TimeUpdateStrategy::ManualDuration(d) if *d == Duration::from_millis(10)));
    }

    #[test]
    fn settle_then_frame_zero_advances_counter() {
        let mut state = CaptureState::default();
        state.assets_ready = true;
        // settle = 1: first armed tick consumes the settle frame, next is sim-frame 0.
        assert_eq!(state.advance_and_current_frame(1), None); // settle frame
        assert_eq!(state.advance_and_current_frame(1), Some(0)); // sim-frame 0
        assert_eq!(state.advance_and_current_frame(1), Some(1)); // sim-frame 1
    }

    #[test]
    fn not_ready_does_not_advance() {
        let mut state = CaptureState::default(); // assets_ready = false
        assert_eq!(state.advance_and_current_frame(2), None);
        assert_eq!(state.sim_frame, 0);
    }
}
```

**Run-to-fail:** `cargo test -p wc-core --lib capture::system`
**Expected failure:** compile error — `CaptureState`, `pin_capture_timestep`, `advance_and_current_frame` undefined.

**Minimal implementation** (full file `crates/wc-core/src/capture/system.rs`):

```rust
//! Deterministic in-app frame-capture system.
//!
//! ## Determinism contract
//!
//! While capturing, the virtual clock is pinned to a fixed `dt`
//! ([`bevy::time::TimeUpdateStrategy::ManualDuration`]) so update *N* maps to
//! sim time *N·dt*. Frame counting starts only once the active sketch is
//! entered AND its required assets are loaded, then waits `settle` frames, so
//! `frame 0` is the first fully-loaded, settled sketch frame. This designs out
//! the wall-clock sampling bug that produced a false "dimmer with a hand"
//! reading: two runs now sample identical points on the gravity-smear triangle
//! wave.
//!
//! ## Release safety
//!
//! This module is `#[cfg(debug_assertions)]`-gated by its parent
//! ([`crate::capture`]). It exists ONLY in debug builds. Capture relies on
//! `debug-assertions = false` in the release/soak profiles — never enable debug
//! assertions there or this system (and its per-frame work) compiles back in.
//!
//! ## Activation
//!
//! Every system here early-returns when [`CaptureConfig`] is absent (i.e.
//! `WC_CAPTURE` was unset), so a normal debug run pays only an `Option<Res<_>>`
//! miss per frame.

use std::io::Write as _;

use bevy::app::AppExit;
use bevy::ecs::message::MessageWriter;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::time::TimeUpdateStrategy;

use super::config::CaptureConfig;
use crate::lifecycle::state::AppState;

/// Capture progress + readiness gate. Inserted once; mutated each frame.
#[derive(Resource, Debug, Default)]
pub struct CaptureState {
    /// True once the active sketch is entered and its required assets are
    /// loaded. Set by [`detect_assets_ready`]; gates all frame counting.
    pub assets_ready: bool,
    /// Settle frames already consumed after `assets_ready` flipped true.
    pub settled: u32,
    /// Current sim-frame index (0 = first fully-loaded, settled frame).
    pub sim_frame: u32,
    /// Number of frames captured so far (drives the exit condition).
    pub captured: usize,
    /// True after `AppExit` has been requested, so we request it only once.
    pub exit_requested: bool,
}

impl CaptureState {
    /// Advance the gate one tick and return the sim-frame index that is "live"
    /// this tick, or `None` while not ready or still settling.
    ///
    /// `settle` is the configured number of settle frames. The first
    /// `settle` armed ticks are consumed silently; the next armed tick is
    /// sim-frame 0, and each subsequent armed tick increments by one.
    pub fn advance_and_current_frame(&mut self, settle: u32) -> Option<u32> {
        if !self.assets_ready {
            return None;
        }
        if self.settled < settle {
            self.settled += 1;
            return None;
        }
        let current = self.sim_frame;
        self.sim_frame += 1;
        Some(current)
    }
}

/// Pin the virtual clock to the configured fixed `dt` for the duration of the
/// capture run, so sim time is `update_index · dt`. Idempotent (re-inserts the
/// same value each frame). No-op without a [`CaptureConfig`].
pub fn pin_capture_timestep(
    config: Option<Res<'_, CaptureConfig>>,
    state: Res<'_, CaptureState>,
    mut commands: Commands<'_, '_>,
) {
    let Some(config) = config else {
        return;
    };
    // Only pin once assets are ready: before that we want the normal clock so
    // asset loading / the OnEnter transition proceed at real pace.
    if state.assets_ready {
        commands.insert_resource(TimeUpdateStrategy::ManualDuration(config.dt));
    }
}

/// Flip [`CaptureState::assets_ready`] to true on the first `Update` where the
/// app has entered a sketch (left `Home`). The sketch's own `OnEnter` has run
/// by then, queuing its asset loads; combined with the `settle` window this is
/// the robust "fully-loaded sketch frame" signal called for by the spec
/// (sketches enter `SketchActivity::Active` only after `OnEnter` completes).
///
/// No-op without a [`CaptureConfig`].
pub fn detect_assets_ready(
    config: Option<Res<'_, CaptureConfig>>,
    state: Option<ResMut<'_, CaptureState>>,
    app_state: Option<Res<'_, State<AppState>>>,
) {
    let (Some(_config), Some(mut state), Some(app_state)) = (config, state, app_state) else {
        return;
    };
    if !state.assets_ready && app_state.get().is_sketch() {
        state.assets_ready = true;
    }
}

/// Per-frame: when the live sim-frame index is in the configured schedule,
/// spawn a [`Screenshot`] of the primary window with a `save_to_disk` observer
/// targeting `<dir>/frame_NNNN.png`. After the last scheduled frame is
/// dispatched, write `run.json` and request [`AppExit`].
///
/// No-op without a [`CaptureConfig`].
pub fn drive_capture(
    config: Option<Res<'_, CaptureConfig>>,
    mut state: Option<ResMut<'_, CaptureState>>,
    mut commands: Commands<'_, '_>,
    mut exit: MessageWriter<'_, AppExit>,
) {
    let (Some(config), Some(state)) = (config.as_ref(), state.as_mut()) else {
        return;
    };

    let Some(current) = state.advance_and_current_frame(config.settle) else {
        return;
    };

    if config.frames.contains(&current) {
        let path = config.dir.join(format!("frame_{current:04}.png"));
        if let Err(err) = std::fs::create_dir_all(&config.dir) {
            tracing::error!(?err, dir = ?config.dir, "capture: cannot create output dir");
        }
        tracing::info!(frame = current, path = ?path, "capture: requesting screenshot");
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
        state.captured += 1;
    }

    // Exit once we have dispatched every scheduled frame. The xtask enforces a
    // wall-clock timeout as a safety net in case a screenshot observer never
    // fires.
    let done = config
        .frames
        .last()
        .is_some_and(|&last| current >= last);
    if done && !state.exit_requested {
        write_run_json(config);
        state.exit_requested = true;
        tracing::info!("capture: schedule complete, requesting AppExit");
        exit.write(AppExit::Success);
    }
}

/// Write the self-describing `run.json` sidecar next to the captured frames.
///
/// Hand-rolled JSON (no serde dependency in wc-core for this) keeps the sidecar
/// minimal; the xtask parses it with `serde_json`. Captures the scheduled
/// frames, `dt` (seconds), and `settle` so a capture is reproducible.
fn write_run_json(config: &CaptureConfig) {
    let frames = config
        .frames
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let json = format!(
        "{{\"frames\":[{}],\"dt_secs\":{},\"settle\":{},\"app_version\":\"{}\"}}\n",
        frames,
        config.dt.as_secs_f64(),
        config.settle,
        env!("CARGO_PKG_VERSION"),
    );
    let path = config.dir.join("run.json");
    match std::fs::File::create(&path).and_then(|mut f| f.write_all(json.as_bytes())) {
        Ok(()) => tracing::info!(path = ?path, "capture: wrote run.json"),
        Err(err) => tracing::error!(?err, path = ?path, "capture: failed to write run.json"),
    }
}
```

**Run-to-pass:** `cargo test -p wc-core --lib capture::system`
**Expected pass:** 3 tests pass. (Requires the `capture` module tree from Task 4's `lib.rs` edit, or the temporary `mod capture { pub mod config; pub mod system; }` block.)

**Commit (operator runs later):**
`git add crates/wc-core/src/capture/system.rs && git commit -m "wc-core/capture: deterministic capture system (settle gate, ManualDuration, screenshot, run.json, AppExit)"`

---

## Task 3 — `DebugToggles` + `WC_DEBUG_*` parser + `DebugPlugin`

**Files:**
- Create `crates/wc-core/src/debug/mod.rs`

**Failing test** (footer of `debug/mod.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_var_present_detects_activation() {
        assert!(!any_debug_var_present(&[]));
        assert!(any_debug_var_present(&[("WC_DEBUG_DISABLE_BLOOM".into(), "1".into())]));
        assert!(!any_debug_var_present(&[("WC_OTHER".into(), "1".into())]));
    }

    #[test]
    fn parses_flags_and_values() {
        let vars = vec![
            ("WC_DEBUG_FORCE_G".to_string(), "8000".to_string()),
            ("WC_DEBUG_DISABLE_SMEAR".to_string(), "1".to_string()),
            ("WC_DEBUG_DISABLE_BLOOM".to_string(), "".to_string()),
            ("WC_DEBUG_SOLID_PARTICLES".to_string(), "ff00ffff".to_string()),
        ];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.force_g, Some(8000.0));
        assert!(t.disable_smear);
        assert!(t.disable_bloom);
        assert!(!t.disable_bone_composite);
        assert_eq!(t.solid_particles, Some([1.0, 0.0, 1.0, 1.0]));
    }

    #[test]
    fn solid_particles_rgb_defaults_alpha_to_one() {
        let vars = vec![("WC_DEBUG_SOLID_PARTICLES".to_string(), "00ff00".to_string())];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.solid_particles, Some([0.0, 1.0, 0.0, 1.0]));
    }

    #[test]
    fn bad_hex_yields_none() {
        let vars = vec![("WC_DEBUG_SOLID_PARTICLES".to_string(), "zzz".to_string())];
        let t = DebugToggles::from_env_vars(&vars);
        assert_eq!(t.solid_particles, None);
    }
}
```

**Run-to-fail:** `cargo test -p wc-core --lib debug`
**Expected failure:** compile error — `DebugToggles`, `any_debug_var_present`, `from_env_vars` undefined.

**Minimal implementation** (full file `crates/wc-core/src/debug/mod.rs`):

```rust
//! Render-stage debug toggles parsed once from the `WC_DEBUG_*` env namespace.
//!
//! ## Role
//!
//! Promotes this sprint's throwaway env-gated render-stage isolation toggles
//! into a first-class resource. Relevant systems/nodes read [`DebugToggles`]
//! instead of calling `std::env` directly (or being patched by hand mid-debug).
//! Toggles consumed by render-graph nodes are mirrored into the render world
//! via [`bevy::render::extract_resource::ExtractResource`] (same pattern as
//! `LinePostParams` / `HandMeshTarget`).
//!
//! ## Activation (Option A hybrid)
//!
//! The module is `#[cfg(debug_assertions)]`-gated by its parent declaration in
//! `lib.rs`. At runtime, [`DebugPlugin`] inserts [`DebugToggles`] ONLY when at
//! least one `WC_DEBUG_*` var is present, so a normal debug run carries no
//! resource at all and every consumer treats `Option<Res<DebugToggles>>::None`
//! as "all toggles off".
//!
//! ## Release safety
//!
//! Compiled out of release. Relies on `debug-assertions = false` in
//! release/soak profiles — never enable assertions there.

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};

/// Curated render-stage isolation toggles. Absent (resource not inserted) when
/// no `WC_DEBUG_*` var is set; each consumer treats `None` as all-off.
#[derive(Resource, Debug, Clone, Copy, PartialEq, ExtractResource)]
pub struct DebugToggles {
    /// `WC_DEBUG_FORCE_G=<f32>`: pin the Line gravity-smear `g_constant`,
    /// eliminating the triangle-wave phase variable.
    pub force_g: Option<f32>,
    /// `WC_DEBUG_DISABLE_SMEAR`: skip the gravity post-process node.
    pub disable_smear: bool,
    /// `WC_DEBUG_DISABLE_BLOOM`: zero/disable the main camera bloom.
    pub disable_bloom: bool,
    /// `WC_DEBUG_DISABLE_BONE_COMPOSITE`: skip the bone-composite node.
    pub disable_bone_composite: bool,
    /// `WC_DEBUG_DISABLE_BONE_CAMERA`: do not spawn the off-screen bone camera.
    pub disable_bone_camera: bool,
    /// `WC_DEBUG_SOLID_PARTICLES=<rgba hex>`: render particles as a flat linear
    /// colour (`[r, g, b, a]`, 0..=1). `a > 0` means "active".
    pub solid_particles: Option<[f32; 4]>,
}

impl DebugToggles {
    /// Build toggles from a list of `(name, value)` env pairs. Recognises only
    /// the `WC_DEBUG_*` names; unknown names are ignored. Flag toggles are true
    /// whenever their var is present (value ignored). Pure for testability.
    pub fn from_env_vars(vars: &[(String, String)]) -> Self {
        let present = |name: &str| vars.iter().any(|(k, _)| k == name);
        let value = |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

        let force_g = value("WC_DEBUG_FORCE_G").and_then(|v| v.trim().parse::<f32>().ok());
        let solid_particles =
            value("WC_DEBUG_SOLID_PARTICLES").and_then(|v| parse_rgba_hex(v.trim()));

        Self {
            force_g,
            disable_smear: present("WC_DEBUG_DISABLE_SMEAR"),
            disable_bloom: present("WC_DEBUG_DISABLE_BLOOM"),
            disable_bone_composite: present("WC_DEBUG_DISABLE_BONE_COMPOSITE"),
            disable_bone_camera: present("WC_DEBUG_DISABLE_BONE_CAMERA"),
            solid_particles,
        }
    }

    /// Read the process environment once and build toggles. Used by
    /// [`DebugPlugin::build`]; the pure [`Self::from_env_vars`] backs the tests.
    fn from_process_env() -> Self {
        let vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("WC_DEBUG_"))
            .collect();
        Self::from_env_vars(&vars)
    }
}

/// True if any `WC_DEBUG_*` var is present — the activation predicate.
pub fn any_debug_var_present(vars: &[(String, String)]) -> bool {
    vars.iter().any(|(k, _)| k.starts_with("WC_DEBUG_"))
}

/// Parse a 6- or 8-digit RGB(A) hex string (no `#`) into linear `[r,g,b,a]` in
/// `0..=1`. 6 digits default alpha to `1.0`. Returns `None` on malformed input.
///
/// Note: the bytes are treated as already-linear channel values (the isolation
/// trick wants a literal flat colour, not an sRGB-decoded one).
fn parse_rgba_hex(hex: &str) -> Option<[f32; 4]> {
    let bytes = match hex.len() {
        6 | 8 => hex,
        _ => return None,
    };
    let component = |i: usize| -> Option<f32> {
        let slice = bytes.get(i..i + 2)?;
        let v = u8::from_str_radix(slice, 16).ok()?;
        Some(f32::from(v) / 255.0)
    };
    let r = component(0)?;
    let g = component(2)?;
    let b = component(4)?;
    let a = if bytes.len() == 8 { component(6)? } else { 1.0 };
    Some([r, g, b, a])
}

/// Inserts [`DebugToggles`] (and its render-world extraction) ONLY when a
/// `WC_DEBUG_*` var is present, then leaves consumers to read the resource.
///
/// ## Signal flow
///
/// Parses the `WC_DEBUG_*` env namespace once at `build` time. When any toggle
/// var is set, inserts [`DebugToggles`] into the main world and registers an
/// [`ExtractResourcePlugin`] so render-graph nodes (gravity smear, bone
/// composite) see the same toggles each frame. When no var is set, inserts
/// nothing — every `Option<Res<DebugToggles>>` consumer sees `None`.
pub struct DebugPlugin;

impl Plugin for DebugPlugin {
    fn build(&self, app: &mut App) {
        let vars: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.starts_with("WC_DEBUG_"))
            .collect();
        if !any_debug_var_present(&vars) {
            return;
        }
        let toggles = DebugToggles::from_process_env();
        tracing::info!(?toggles, "WC_DEBUG_* active");
        app.insert_resource(toggles);
        app.add_plugins(ExtractResourcePlugin::<DebugToggles>::default());
    }
}
```

**Run-to-pass:** `cargo test -p wc-core --lib debug`
**Expected pass:** 4 tests pass.

**Commit (operator runs later):**
`git add crates/wc-core/src/debug/mod.rs && git commit -m "wc-core/debug: DebugToggles + WC_DEBUG_* parse-once + DebugPlugin"`

---

## Task 4 — Wire `capture` + `debug` modules into `CorePlugin` (compile-gated)

**Files:**
- Modify `crates/wc-core/src/lib.rs`

**Failing test** (add to the existing `#[cfg(test)] mod tests` in `lib.rs`):

```rust
    #[test]
    #[cfg(debug_assertions)]
    fn core_plugin_does_not_insert_debug_toggles_without_env() {
        // No WC_DEBUG_* set in the test process → DebugPlugin inserts nothing.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(CorePlugin);
        assert!(app.world().get_resource::<crate::debug::DebugToggles>().is_none());
        // CaptureState is only meaningful with WC_CAPTURE; CorePlugin must still build.
    }
```

**Run-to-fail:** `cargo test -p wc-core --lib core_plugin_does_not_insert_debug_toggles_without_env`
**Expected failure:** compile error — `crate::debug` / `crate::capture` modules not declared (and `CapturePlugin`/`DebugPlugin` not registered).

**Minimal implementation** — edit `crates/wc-core/src/lib.rs`:

Replace the module-declaration block:

```rust
pub mod audio;
pub mod input;
pub mod lifecycle;
pub mod settings;
pub mod sketch;
pub mod ui;
```

with (capture + debug are debug-only; see each module's release-safety note):

```rust
pub mod audio;
// Visual-debugging scaffold: compiled out of release entirely (Option A hybrid
// gating). Both modules rely on `debug-assertions = false` in the release/soak
// profiles — see each module's docs and the guard comment on
// `[profile.release]` in the workspace `Cargo.toml`.
#[cfg(debug_assertions)]
pub mod capture;
#[cfg(debug_assertions)]
pub mod debug;
pub mod input;
pub mod lifecycle;
pub mod settings;
pub mod sketch;
pub mod ui;
```

And the `capture` module needs a root file declaring its children. Create `crates/wc-core/src/capture/mod.rs`:

```rust
//! In-app deterministic frame-capture scaffold (debug builds only).
//!
//! ## Role
//!
//! Activated at runtime by the `WC_CAPTURE` env var (parsed once into
//! [`config::CaptureConfig`]). Pins a fixed virtual-time `dt`, waits for the
//! sketch's assets to be ready plus a settle window, then screenshots the
//! primary window at the scheduled sim-frame indices, writes a self-describing
//! `run.json`, and requests `AppExit`. See [`system`] for the determinism
//! contract.
//!
//! ## Release safety (Option A hybrid gating)
//!
//! This whole module is `#[cfg(debug_assertions)]`-gated at its `lib.rs`
//! declaration. It must never compile into release: capture relies on
//! `debug-assertions = false` in the release/soak profiles. If you ever flip
//! `debug-assertions = true` on a release-class profile, this system and its
//! per-frame work reappear — don't.

pub mod config;
pub mod system;

use bevy::prelude::*;

use config::{parse_wc_capture, CaptureConfig};
use system::{detect_assets_ready, drive_capture, pin_capture_timestep, CaptureState};

/// Parses `WC_CAPTURE` once at build and, when present, wires the capture
/// systems + state. When `WC_CAPTURE` is unset the plugin inserts nothing and
/// every capture system early-returns on its missing [`CaptureConfig`].
///
/// ## Signal flow
///
/// `WC_CAPTURE` -> [`CaptureConfig`] (parse-once). Each `Update`:
/// [`detect_assets_ready`] flips the readiness gate on sketch entry;
/// [`pin_capture_timestep`] pins `Time<Virtual>` to the fixed `dt` once ready;
/// [`drive_capture`] advances the settle/frame counter, screenshots scheduled
/// frames, and requests `AppExit` after the last one.
pub struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        let Ok(raw) = std::env::var("WC_CAPTURE") else {
            return;
        };
        match parse_wc_capture(&raw) {
            Ok(config) => {
                tracing::info!(?config, "WC_CAPTURE active");
                app.insert_resource(config);
                app.init_resource::<CaptureState>();
                app.add_systems(
                    Update,
                    (detect_assets_ready, pin_capture_timestep, drive_capture).chain(),
                );
            }
            Err(err) => tracing::error!(%err, "WC_CAPTURE parse failed; capture disabled"),
        }
    }
}
```

Then register both plugins in `CorePlugin::build` — insert after `SettingsPlugin` (the existing line 37) and before `WaveConductorUiPlugin`:

```rust
impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
        app.add_plugins(input::HandTrackingPlugin);
        app.add_plugins(audio::AudioPlugin);
        app.add_plugins(settings::SettingsPlugin);
        // Visual-debugging scaffold — debug builds only (compiled out of
        // release). DebugPlugin inserts DebugToggles only when a WC_DEBUG_* var
        // is set; CapturePlugin wires the capture systems only when WC_CAPTURE
        // is set. A normal debug run with neither env carries essentially
        // nothing.
        #[cfg(debug_assertions)]
        app.add_plugins(capture::CapturePlugin);
        #[cfg(debug_assertions)]
        app.add_plugins(debug::DebugPlugin);
        app.add_plugins(ui::WaveConductorUiPlugin);
    }
}
```

(If you used a temporary `mod capture { … }` block in Tasks 1–2, delete it now — the real `capture/mod.rs` replaces it.)

**Run-to-pass:**
`cargo test -p wc-core --lib core_plugin_does_not_insert_debug_toggles_without_env`
then `cargo test -p wc-core --lib capture:: debug::`
**Expected pass:** new test passes; capture + debug unit tests still pass; `core_plugin_builds_without_panicking` still passes.

**Commit (operator runs later):**
`git add crates/wc-core/src/lib.rs crates/wc-core/src/capture/mod.rs && git commit -m "wc-core: register CapturePlugin + DebugPlugin in CorePlugin (debug-only)"`

---

## Task 5 — xtask deps + scenarios loader

**Files:**
- Modify `xtask/Cargo.toml`
- Create `xtask/src/capture/scenarios.rs`
- Create `tests/visual/scenarios.toml`

**Failing test** (footer of `scenarios.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scenarios_toml() {
        let toml = r#"
            [scenarios.line-synthetic]
            sketch = "line"
            provider = "synthetic"
            config = "clean"
            frames = [30, 60, 120]
            dt = 0.016666667

            [scenarios.line-synthetic.debug]
            FORCE_G = "8000"
            DISABLE_BLOOM = "1"
        "#;
        let scenarios: Scenarios = toml::from_str(toml).unwrap();
        let s = scenarios.get("line-synthetic").unwrap();
        assert_eq!(s.sketch, "line");
        assert_eq!(s.provider, "synthetic");
        assert_eq!(s.config, "clean");
        assert_eq!(s.frames, vec![30, 60, 120]);
        assert_eq!(s.debug.get("FORCE_G").map(String::as_str), Some("8000"));
    }

    #[test]
    fn names_are_listed_sorted() {
        let toml = r#"
            [scenarios.zebra]
            sketch = "line"
            provider = "mock"
            config = "clean"
            frames = [1]
            [scenarios.alpha]
            sketch = "line"
            provider = "mock"
            config = "clean"
            frames = [1]
        "#;
        let scenarios: Scenarios = toml::from_str(toml).unwrap();
        assert_eq!(scenarios.names(), vec!["alpha".to_string(), "zebra".to_string()]);
    }
}
```

**Run-to-fail:** `cargo test -p xtask scenarios`
**Expected failure:** compile error — `toml`/`serde` not in xtask deps; `Scenarios`/`Scenario` undefined.

**Minimal implementation:**

Edit `xtask/Cargo.toml` `[dependencies]`:

```toml
[dependencies]
clap = { workspace = true }
walkdir = { workspace = true }
regex = { workspace = true }
image = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
```

Create `xtask/src/capture/scenarios.rs`:

```rust
//! Scenario table loaded from `tests/visual/scenarios.toml`.
//!
//! A scenario names a deterministic launch: which sketch, hand provider, config
//! isolation, `WC_DEBUG_*` toggles, captured frame indices, and optional `dt`.
//! Baselines key off the scenario name.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Top-level `scenarios.toml` document: `[scenarios.<name>]` tables.
#[derive(Debug, Deserialize)]
pub struct Scenarios {
    /// Map of scenario name → definition. `BTreeMap` so `names()` is sorted.
    pub scenarios: BTreeMap<String, Scenario>,
}

impl Scenarios {
    /// Look up a scenario by name.
    pub fn get(&self, name: &str) -> Option<&Scenario> {
        self.scenarios.get(name)
    }

    /// All scenario names, sorted (for `--list`).
    pub fn names(&self) -> Vec<String> {
        self.scenarios.keys().cloned().collect()
    }
}

/// One named capture scenario.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    /// Sketch name → `WAVECONDUCTOR_START_SKETCH`.
    pub sketch: String,
    /// Hand provider → `WAVECONDUCTOR_HAND_PROVIDER` (`synthetic`, `mock`, …).
    pub provider: String,
    /// `"clean"` (fresh temp config dir) or a path pinned via
    /// `WAVECONDUCTOR_CONFIG_DIR`.
    pub config: String,
    /// `WC_DEBUG_*` toggles as `KEY = "value"` (KEY without the `WC_DEBUG_`
    /// prefix; the launcher re-prefixes).
    #[serde(default)]
    pub debug: BTreeMap<String, String>,
    /// Sim-frame indices to capture.
    pub frames: Vec<u32>,
    /// Optional fixed timestep in seconds (default `1/60` in the app).
    #[serde(default)]
    pub dt: Option<f64>,
}
```

Create `tests/visual/scenarios.toml`:

```toml
# Visual-capture scenarios for `cargo xtask capture`. See tests/visual/CLAUDE.md.
#
# Each [scenarios.<name>] is a deterministic launch. Baselines live under
# tests/visual/baselines/<name>/. `config = "clean"` means a fresh temp config
# dir (no stale on-disk settings); a path value pins WAVECONDUCTOR_CONFIG_DIR.

[scenarios.line-synthetic]
sketch = "line"
provider = "synthetic"
config = "clean"
frames = [30, 60, 120, 240]
dt = 0.016666667

[scenarios.line-synthetic-no-bloom]
sketch = "line"
provider = "synthetic"
config = "clean"
frames = [30, 60, 120, 240]
dt = 0.016666667

[scenarios.line-synthetic-no-bloom.debug]
DISABLE_BLOOM = "1"
```

**Run-to-pass:** `cargo test -p xtask scenarios`
**Expected pass:** 2 tests pass. (`xtask/src/capture/scenarios.rs` is referenced once `mod scenarios;` is declared inside `capture.rs` in Task 8; until then add a temporary `mod capture { pub mod scenarios; }` in `xtask/src/main.rs`, or land Task 8's `mod capture;` declaration first. Keeping the parser tests green only needs the module compiled.)

**Commit (operator runs later):**
`git add xtask/Cargo.toml xtask/src/capture/scenarios.rs tests/visual/scenarios.toml && git commit -m "xtask/capture: add image+serde+toml deps; scenarios.toml loader"`

---

## Task 6 — Metrics (region means, uniformity, frame-delta)

**Files:**
- Create `xtask/src/capture/metrics.rs`

**Failing test** (footer of `metrics.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    #[test]
    fn region_mean_of_solid_image_is_that_color() {
        let img = solid(10, 10, [100, 150, 200]);
        let m = region_mean(&img, Region::Full);
        assert!((m[0] - 100.0).abs() < 0.01);
        assert!((m[1] - 150.0).abs() < 0.01);
        assert!((m[2] - 200.0).abs() < 0.01);
    }

    #[test]
    fn uniform_image_has_zero_std() {
        let img = solid(8, 8, [40, 40, 40]);
        assert!(global_std(&img) < 0.01);
    }

    #[test]
    fn frame_delta_identical_is_zero_different_is_positive() {
        let a = solid(4, 4, [10, 10, 10]);
        let b = solid(4, 4, [10, 10, 10]);
        let c = solid(4, 4, [20, 10, 10]);
        assert!(frame_mean_abs_delta(&a, &b) < 0.01);
        assert!((frame_mean_abs_delta(&a, &c) - 2.5).abs() < 0.01); // 10/4 channels avg
    }

    #[test]
    fn center_region_excludes_borders() {
        // Border red, center green; center mean should be ~green.
        let mut img = solid(10, 10, [255, 0, 0]);
        for y in 3..7 {
            for x in 3..7 {
                img.put_pixel(x, y, Rgba([0, 255, 0, 255]));
            }
        }
        let m = region_mean(&img, Region::Center);
        assert!(m[1] > m[0]); // green dominates the center
    }
}
```

**Run-to-fail:** `cargo test -p xtask metrics`
**Expected failure:** compile error — `region_mean`, `Region`, `global_std`, `frame_mean_abs_delta` undefined.

**Minimal implementation** (full file `xtask/src/capture/metrics.rs`):

```rust
//! Pure per-frame image metrics over captured PNGs (no GPU, no app).
//!
//! These cheap metrics tell the agent *which* frames to open and view; the
//! agent applies the visual judgment (no LLM/vision API). All metrics operate
//! on the decoded `RgbaImage`; means are in 0..=255 channel units.

use image::RgbaImage;
use serde::Serialize;

/// Which area of the frame a region metric covers.
#[derive(Debug, Clone, Copy)]
pub enum Region {
    /// The whole frame.
    Full,
    /// The centre 50% box (excludes a 25% border on every side).
    Center,
}

/// Per-frame metric bundle emitted to `metrics.json`.
#[derive(Debug, Clone, Serialize)]
pub struct FrameMetrics {
    /// Sim-frame index this metric describes.
    pub frame: u32,
    /// Mean RGB over the full frame (0..=255).
    pub full_mean: [f64; 3],
    /// Mean RGB over the centre box (0..=255).
    pub center_mean: [f64; 3],
    /// Global luma standard deviation (uniformity; low = flat frame).
    pub global_std: f64,
    /// Mean absolute per-channel delta vs the previous captured frame, or
    /// `null` for the first frame (frozen-vs-animated signal).
    pub delta_prev: Option<f64>,
}

/// Mean RGB over a region, in 0..=255 channel units.
pub fn region_mean(img: &RgbaImage, region: Region) -> [f64; 3] {
    let (w, h) = img.dimensions();
    let (x0, y0, x1, y1) = match region {
        Region::Full => (0, 0, w, h),
        Region::Center => (w / 4, h / 4, w - w / 4, h - h / 4),
    };
    let mut sum = [0.0_f64; 3];
    let mut count = 0.0_f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y).0;
            sum[0] += f64::from(p[0]);
            sum[1] += f64::from(p[1]);
            sum[2] += f64::from(p[2]);
            count += 1.0;
        }
    }
    if count == 0.0 {
        return [0.0; 3];
    }
    [sum[0] / count, sum[1] / count, sum[2] / count]
}

/// Global luma standard deviation (Rec. 601 luma), a uniformity measure.
pub fn global_std(img: &RgbaImage) -> f64 {
    let lumas: Vec<f64> = img
        .pixels()
        .map(|p| {
            0.299 * f64::from(p.0[0]) + 0.587 * f64::from(p.0[1]) + 0.114 * f64::from(p.0[2])
        })
        .collect();
    if lumas.is_empty() {
        return 0.0;
    }
    let n = lumas.len() as f64;
    let mean = lumas.iter().sum::<f64>() / n;
    let var = lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / n;
    var.sqrt()
}

/// Mean absolute per-channel difference between two same-size frames (0..=255).
/// Returns `f64::INFINITY` if dimensions differ (caller flags as a hard change).
pub fn frame_mean_abs_delta(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return f64::INFINITY;
    }
    let mut sum = 0.0_f64;
    let mut count = 0.0_f64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            sum += (f64::from(pa.0[c]) - f64::from(pb.0[c])).abs();
            count += 1.0;
        }
    }
    if count == 0.0 {
        0.0
    } else {
        sum / count
    }
}
```

Note the cast `lumas.len() as f64`: `usize → f64` is not expressible via `From`/`TryFrom` losslessly, and it is in xtask (a CLI tool). xtask inherits the workspace `as_conversions = "warn"` lint, so add a scoped allow at the top of the file with a reason:

```rust
#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "metric sums need usize->f64 for averaging; precision loss is acceptable for image stats"
)]
```

**Run-to-pass:** `cargo test -p xtask metrics`
**Expected pass:** 4 tests pass.

**Commit (operator runs later):**
`git add xtask/src/capture/metrics.rs && git commit -m "xtask/capture: pure per-frame image metrics"`

---

## Task 7 — Baseline diff (mean abs diff, % over threshold, tolerance verdict)

**Files:**
- Create `xtask/src/capture/diff.rs`

**Failing test** (footer of `diff.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(8, 8, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    #[test]
    fn identical_images_diff_zero() {
        let a = solid([50, 50, 50]);
        let d = diff_frames(&a, &a, 10);
        assert!(d.mean_abs_diff < 0.01);
        assert!(d.pct_over_threshold < 0.01);
    }

    #[test]
    fn known_delta_matches_expected() {
        let a = solid([10, 10, 10]);
        let b = solid([20, 10, 10]); // +10 on R only -> mean over 3 ch = 10/3
        let d = diff_frames(&a, &b, 5);
        assert!((d.mean_abs_diff - (10.0 / 3.0)).abs() < 0.01);
        assert!((d.pct_over_threshold - 100.0).abs() < 0.01); // every pixel changed (R by 10 > 5)
    }

    #[test]
    fn tolerance_boundary_passes_and_fails() {
        let a = solid([10, 10, 10]);
        let b = solid([12, 10, 10]); // small change
        let d = diff_frames(&a, &b, 5);
        assert!(d.passes(2.0)); // mean ~0.67 < 2.0 tolerance
        assert!(!d.passes(0.1)); // mean ~0.67 > 0.1 tolerance
    }

    #[test]
    fn size_mismatch_is_max_diff() {
        let a = solid([10, 10, 10]);
        let b = RgbaImage::from_pixel(4, 4, Rgba([10, 10, 10, 255]));
        let d = diff_frames(&a, &b, 5);
        assert!(d.mean_abs_diff.is_infinite());
        assert!(!d.passes(1000.0));
    }
}
```

**Run-to-fail:** `cargo test -p xtask diff`
**Expected failure:** compile error — `diff_frames`, `FrameDiff` undefined.

**Minimal implementation** (full file `xtask/src/capture/diff.rs`):

```rust
//! Tolerance-based baseline diff between a captured frame and its baseline.
//!
//! Not a pixel-perfect gate: GPU/driver float differences make exact matching
//! brittle. We report mean per-pixel absolute difference and the percentage of
//! pixels whose max-channel delta exceeds a per-pixel threshold; a frame passes
//! when its mean abs diff is within tolerance. The agent reviews flagged frames.

#![allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "diff sums need usize->f64 for averaging; precision loss is acceptable for image stats"
)]

use image::RgbaImage;
use serde::Serialize;

/// Diff verdict for one captured frame vs its baseline.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FrameDiff {
    /// Mean per-channel absolute difference (0..=255); `INFINITY` on size
    /// mismatch.
    pub mean_abs_diff: f64,
    /// Percentage of pixels whose max-channel delta exceeds `threshold`.
    pub pct_over_threshold: f64,
}

impl FrameDiff {
    /// Whether this frame is within `tolerance` (max acceptable mean abs diff).
    pub fn passes(&self, tolerance: f64) -> bool {
        self.mean_abs_diff <= tolerance
    }
}

/// Compute the diff between a captured frame and its baseline. `threshold` is
/// the per-pixel max-channel delta (0..=255) above which a pixel is "changed".
pub fn diff_frames(current: &RgbaImage, baseline: &RgbaImage, threshold: u8) -> FrameDiff {
    if current.dimensions() != baseline.dimensions() {
        return FrameDiff {
            mean_abs_diff: f64::INFINITY,
            pct_over_threshold: 100.0,
        };
    }
    let mut sum = 0.0_f64;
    let mut channels = 0.0_f64;
    let mut changed_pixels = 0.0_f64;
    let mut total_pixels = 0.0_f64;
    let thresh = f64::from(threshold);
    for (pc, pb) in current.pixels().zip(baseline.pixels()) {
        let mut max_delta = 0.0_f64;
        for c in 0..3 {
            let d = (f64::from(pc.0[c]) - f64::from(pb.0[c])).abs();
            sum += d;
            channels += 1.0;
            if d > max_delta {
                max_delta = d;
            }
        }
        total_pixels += 1.0;
        if max_delta > thresh {
            changed_pixels += 1.0;
        }
    }
    let mean_abs_diff = if channels == 0.0 { 0.0 } else { sum / channels };
    let pct_over_threshold = if total_pixels == 0.0 {
        0.0
    } else {
        100.0 * changed_pixels / total_pixels
    };
    FrameDiff {
        mean_abs_diff,
        pct_over_threshold,
    }
}
```

**Run-to-pass:** `cargo test -p xtask diff`
**Expected pass:** 4 tests pass.

**Commit (operator runs later):**
`git add xtask/src/capture/diff.rs && git commit -m "xtask/capture: tolerance-based baseline diff"`

---

## Task 8 — `cargo xtask capture` subcommand + main/manifest sync

**Files:**
- Create `xtask/src/capture.rs`
- Modify `xtask/src/main.rs`
- Modify `xtask/src/manifest.rs`

**Failing test** (footer of `xtask/src/capture.rs` — unit-tests the pure env-assembly helper, which needs no app launch):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::scenarios::Scenario;
    use std::collections::BTreeMap;

    fn scenario() -> Scenario {
        Scenario {
            sketch: "line".into(),
            provider: "synthetic".into(),
            config: "clean".into(),
            debug: BTreeMap::from([("FORCE_G".into(), "8000".into())]),
            frames: vec![30, 60],
            dt: Some(0.016666667),
        }
    }

    #[test]
    fn builds_wc_capture_string() {
        let s = scenario();
        let wc = build_wc_capture(&s, std::path::Path::new("target/capture/x"));
        assert!(wc.starts_with("dir=target/capture/x;frames=30,60"));
        assert!(wc.contains("dt=0.016666667"));
    }

    #[test]
    fn cli_debug_overrides_merge_over_scenario() {
        let s = scenario();
        let overrides = vec!["FORCE_G=4000".to_string(), "DISABLE_SMEAR=1".to_string()];
        let merged = merge_debug(&s, &overrides);
        assert_eq!(merged.get("FORCE_G").map(String::as_str), Some("4000")); // overridden
        assert_eq!(merged.get("DISABLE_SMEAR").map(String::as_str), Some("1")); // added
    }

    #[test]
    fn env_pairs_prefix_wc_debug() {
        let merged = BTreeMap::from([("FORCE_G".to_string(), "8000".to_string())]);
        let pairs = debug_env_pairs(&merged);
        assert!(pairs.contains(&("WC_DEBUG_FORCE_G".to_string(), "8000".to_string())));
    }
}
```

**Run-to-fail:** `cargo test -p xtask capture::tests`
**Expected failure:** compile error — `build_wc_capture`, `merge_debug`, `debug_env_pairs` undefined.

**Minimal implementation** (full file `xtask/src/capture.rs`):

```rust
//! `cargo xtask capture <scenario>` — orchestrate a deterministic capture run,
//! compute metrics, diff baselines, and report.
//!
//! Independent of `wc-core`/`wc-sketches`: this shells out to the DEBUG
//! `waveconductor` binary (`cargo run -p waveconductor`), teeing its output to
//! `<dir>/app.log`, then reads the PNGs + `run.json` the app wrote.

pub mod diff;
pub mod metrics;
pub mod scenarios;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::Args as ClapArgs;

use diff::diff_frames;
use metrics::{global_std, region_mean, FrameMetrics, Region};
use scenarios::{Scenario, Scenarios};

/// Per-pixel max-channel delta above which a pixel counts as changed.
const PIXEL_THRESHOLD: u8 = 12;

/// Mean-abs-diff tolerance (0..=255) below which a frame passes the baseline.
const DIFF_TOLERANCE: f64 = 6.0;

/// Wall-clock safety timeout for the launched app (seconds).
const LAUNCH_TIMEOUT_SECS: u64 = 90;

/// Arguments for the capture subcommand.
#[derive(ClapArgs)]
pub struct Args {
    /// Scenario name from `tests/visual/scenarios.toml`. Omit with `--list`.
    pub scenario: Option<String>,
    /// Copy the freshly-captured frames into the baseline dir (no diff gate).
    #[arg(long)]
    pub update_baselines: bool,
    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    pub json: bool,
    /// Launch the scenario for hands-on inspection (no capture); quit after N
    /// seconds (default 10). Runs the normal variable-dt clock.
    #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "10")]
    pub watch: Option<u64>,
    /// List available scenarios and exit.
    #[arg(long)]
    pub list: bool,
    /// Ad-hoc `WC_DEBUG_*` overrides as `KEY=VAL` (KEY without the prefix).
    #[arg(long = "debug", value_name = "KEY=VAL")]
    pub debug: Vec<String>,
}

/// Execute the capture subcommand.
pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let root = workspace_root();
    let scenarios = load_scenarios(&root)?;

    if args.list {
        print_list(&scenarios, args.json);
        return Ok(());
    }

    let name = args
        .scenario
        .as_deref()
        .ok_or("capture: a scenario name is required (or use --list)")?;
    let scenario = scenarios
        .get(name)
        .ok_or_else(|| format!("capture: unknown scenario {name:?}; try --list"))?;

    let out_dir = root.join("target").join("capture").join(name);
    std::fs::create_dir_all(&out_dir)?;

    if let Some(secs) = args.watch {
        return run_watch(&root, scenario, secs);
    }

    launch(&root, scenario, &out_dir, &args.debug)?;

    let report = analyze(&root, name, scenario, &out_dir)?;

    if args.update_baselines {
        update_baselines(&root, name, scenario, &out_dir)?;
        if args.json {
            println!("{{\"scenario\":\"{name}\",\"updated_baselines\":true}}");
        } else {
            println!("Updated baselines for {name}.");
        }
        return Ok(());
    }

    let passed = report.frames.iter().all(|f| f.passed);
    if args.json {
        print_json_report(name, &out_dir, &report);
    } else {
        print_human_report(name, &report);
    }
    if passed {
        Ok(())
    } else {
        Err(format!("capture: {name} regressed beyond tolerance").into())
    }
}

/// Assemble the `WC_CAPTURE` env value for a scenario + output dir.
pub fn build_wc_capture(scenario: &Scenario, out_dir: &Path) -> String {
    let frames = scenario
        .frames
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut wc = format!("dir={};frames={}", out_dir.display(), frames);
    if let Some(dt) = scenario.dt {
        wc.push_str(&format!(";dt={dt}"));
    }
    wc
}

/// Merge CLI `--debug KEY=VAL` overrides over a scenario's `debug` table. CLI
/// values win; new keys are added.
pub fn merge_debug(scenario: &Scenario, overrides: &[String]) -> BTreeMap<String, String> {
    let mut merged = scenario.debug.clone();
    for ov in overrides {
        if let Some((k, v)) = ov.split_once('=') {
            merged.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    merged
}

/// Turn a merged debug table into `(WC_DEBUG_<KEY>, VAL)` env pairs.
pub fn debug_env_pairs(merged: &BTreeMap<String, String>) -> Vec<(String, String)> {
    merged
        .iter()
        .map(|(k, v)| (format!("WC_DEBUG_{k}"), v.clone()))
        .collect()
}

// ---- private orchestration helpers --------------------------------------

/// Workspace root: parent of the xtask crate dir (`CARGO_MANIFEST_DIR`).
fn workspace_root() -> PathBuf {
    std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .and_then(|d| PathBuf::from(d).parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Load `tests/visual/scenarios.toml`.
fn load_scenarios(root: &Path) -> Result<Scenarios, Box<dyn std::error::Error>> {
    let path = root.join("tests").join("visual").join("scenarios.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("capture: cannot read {}: {e}", path.display()))?;
    Ok(toml::from_str(&text)?)
}

/// Launch the debug binary with scenario env + capture schedule, teeing
/// stdout+stderr to `<dir>/app.log`, enforcing a wall-clock timeout.
fn launch(
    root: &Path,
    scenario: &Scenario,
    out_dir: &Path,
    cli_debug: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .args(["run", "-p", "waveconductor"])
        .env("WAVECONDUCTOR_START_SKETCH", &scenario.sketch)
        .env("WAVECONDUCTOR_HAND_PROVIDER", &scenario.provider)
        .env("WC_CAPTURE", build_wc_capture(scenario, out_dir));

    // Config isolation: a fresh temp dir for `config = "clean"`, else a pinned
    // path. The temp dir is created under the output dir so it is inspectable.
    if scenario.config == "clean" {
        let clean = out_dir.join("clean-config");
        std::fs::create_dir_all(&clean)?;
        cmd.env("WAVECONDUCTOR_CONFIG_DIR", &clean);
    } else {
        cmd.env("WAVECONDUCTOR_CONFIG_DIR", &scenario.config);
    }

    for (k, v) in debug_env_pairs(&merge_debug(scenario, cli_debug)) {
        cmd.env(k, v);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    // Drain both pipes into app.log. Threads avoid a pipe-buffer deadlock.
    let log_path = out_dir.join("app.log");
    let log = std::sync::Arc::new(std::sync::Mutex::new(std::fs::File::create(&log_path)?));
    let mut handles = Vec::new();
    for pipe in [child.stdout.take(), child.stderr.take()] {
        if let Some(mut reader) = pipe {
            let log = std::sync::Arc::clone(&log);
            handles.push(std::thread::spawn(move || {
                use std::io::Read as _;
                let mut buf = [0_u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut f) = log.lock() {
                        let _ = f.write_all(&buf[..n]);
                    }
                }
            }));
        }
    }

    // Wall-clock timeout safety net (the app self-exits via AppExit normally).
    let start = std::time::Instant::now();
    loop {
        if let Some(_status) = child.try_wait()? {
            break;
        }
        if start.elapsed().as_secs() > LAUNCH_TIMEOUT_SECS {
            let _ = child.kill();
            return Err(format!(
                "capture: app did not exit within {LAUNCH_TIMEOUT_SECS}s; see {}",
                log_path.display()
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

/// `--watch`: launch for hands-on inspection (no `WC_CAPTURE`), kill after N s.
fn run_watch(
    root: &Path,
    scenario: &Scenario,
    secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .args(["run", "-p", "waveconductor"])
        .env("WAVECONDUCTOR_START_SKETCH", &scenario.sketch)
        .env("WAVECONDUCTOR_HAND_PROVIDER", &scenario.provider);
    let mut child = cmd.spawn()?;
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < secs {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    Ok(())
}

/// One frame's report row.
struct FrameReport {
    frame: u32,
    metrics: FrameMetrics,
    mean_abs_diff: Option<f64>,
    passed: bool,
    current_path: PathBuf,
    baseline_path: Option<PathBuf>,
}

/// Aggregate report.
struct Report {
    frames: Vec<FrameReport>,
}

/// Read PNGs + run.json, compute metrics + baseline diffs.
fn analyze(
    root: &Path,
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
) -> Result<Report, Box<dyn std::error::Error>> {
    let baseline_dir = root.join("tests").join("visual").join("baselines").join(name);
    let mut frames = Vec::new();
    let mut prev: Option<image::RgbaImage> = None;

    // Write metrics.json alongside the report.
    let mut metrics_out: Vec<FrameMetrics> = Vec::new();

    for &frame in &scenario.frames {
        let current_path = out_dir.join(format!("frame_{frame:04}.png"));
        let current = image::open(&current_path)
            .map_err(|e| format!("capture: cannot read {}: {e}", current_path.display()))?
            .to_rgba8();

        let delta_prev = prev.as_ref().map(|p| metrics::frame_mean_abs_delta(p, &current));
        let fm = FrameMetrics {
            frame,
            full_mean: region_mean(&current, Region::Full),
            center_mean: region_mean(&current, Region::Center),
            global_std: global_std(&current),
            delta_prev,
        };
        metrics_out.push(fm.clone());

        let baseline_path = baseline_dir.join(format!("frame_{frame:04}.png"));
        let (mean_abs_diff, passed, baseline_ref) = if baseline_path.exists() {
            let baseline = image::open(&baseline_path)?.to_rgba8();
            let d = diff_frames(&current, &baseline, PIXEL_THRESHOLD);
            (Some(d.mean_abs_diff), d.passes(DIFF_TOLERANCE), Some(baseline_path))
        } else {
            // No baseline yet → cannot regress; flag for the agent to review.
            (None, true, None)
        };

        frames.push(FrameReport {
            frame,
            metrics: fm,
            mean_abs_diff,
            passed,
            current_path,
            baseline_path: baseline_ref,
        });
        prev = Some(current);
    }

    let metrics_path = out_dir.join("metrics.json");
    let mut f = std::fs::File::create(&metrics_path)?;
    f.write_all(serde_json::to_string_pretty(&metrics_out)?.as_bytes())?;

    Ok(Report { frames })
}

/// Copy captured frames into the baseline dir (plain committed PNGs, no LFS).
fn update_baselines(
    root: &Path,
    name: &str,
    scenario: &Scenario,
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let baseline_dir = root.join("tests").join("visual").join("baselines").join(name);
    std::fs::create_dir_all(&baseline_dir)?;
    for &frame in &scenario.frames {
        let src = out_dir.join(format!("frame_{frame:04}.png"));
        let dst = baseline_dir.join(format!("frame_{frame:04}.png"));
        std::fs::copy(&src, &dst)
            .map_err(|e| format!("capture: cannot copy baseline {}: {e}", dst.display()))?;
    }
    Ok(())
}

fn print_list(scenarios: &Scenarios, json: bool) {
    if json {
        let names: Vec<String> = scenarios
            .names()
            .into_iter()
            .map(|n| format!("\"{n}\""))
            .collect();
        println!("[{}]", names.join(","));
    } else {
        println!("SCENARIOS");
        for n in scenarios.names() {
            println!("  {n}");
        }
    }
}

fn print_human_report(name: &str, report: &Report) {
    println!("CAPTURE {name}");
    println!("{:<8} {:<22} {:<10} {:<10} {}", "FRAME", "FULL_MEAN(RGB)", "STD", "DIFF", "VERDICT");
    for f in &report.frames {
        let diff = f
            .mean_abs_diff
            .map_or_else(|| "n/a".to_string(), |d| format!("{d:.2}"));
        let verdict = if f.baseline_path.is_none() {
            "NEW (review)"
        } else if f.passed {
            "pass"
        } else {
            "REGRESS (open)"
        };
        println!(
            "{:<8} {:<22} {:<10.2} {:<10} {}",
            f.frame,
            format!(
                "{:.0},{:.0},{:.0}",
                f.metrics.full_mean[0], f.metrics.full_mean[1], f.metrics.full_mean[2]
            ),
            f.metrics.global_std,
            diff,
            verdict,
        );
    }
    let to_open: Vec<String> = report
        .frames
        .iter()
        .filter(|f| !f.passed || f.baseline_path.is_none())
        .map(|f| f.current_path.display().to_string())
        .collect();
    if to_open.is_empty() {
        println!("All frames within tolerance.");
    } else {
        println!("Open & judge these frames:");
        for p in to_open {
            println!("  {p}");
        }
    }
}

fn print_json_report(name: &str, out_dir: &Path, report: &Report) {
    // Hand-rolled JSON so the shape is explicit and stable for the agent.
    let mut frames_json = Vec::new();
    for f in &report.frames {
        let diff = f
            .mean_abs_diff
            .map_or_else(|| "null".to_string(), |d| format!("{d:.4}"));
        let baseline = f
            .baseline_path
            .as_ref()
            .map_or_else(|| "null".to_string(), |p| format!("\"{}\"", p.display()));
        frames_json.push(format!(
            "{{\"frame\":{},\"full_mean\":[{:.2},{:.2},{:.2}],\"center_mean\":[{:.2},{:.2},{:.2}],\"global_std\":{:.4},\"mean_abs_diff\":{},\"passed\":{},\"current\":\"{}\",\"baseline\":{}}}",
            f.frame,
            f.metrics.full_mean[0], f.metrics.full_mean[1], f.metrics.full_mean[2],
            f.metrics.center_mean[0], f.metrics.center_mean[1], f.metrics.center_mean[2],
            f.metrics.global_std,
            diff,
            f.passed,
            f.current_path.display(),
            baseline,
        ));
    }
    let open: Vec<String> = report
        .frames
        .iter()
        .filter(|f| !f.passed || f.baseline_path.is_none())
        .map(|f| format!("\"{}\"", f.current_path.display()))
        .collect();
    let passed = report.frames.iter().all(|f| f.passed);
    println!(
        "{{\"scenario\":\"{}\",\"dir\":\"{}\",\"passed\":{},\"frames\":[{}],\"open_for_review\":[{}]}}",
        name,
        out_dir.display(),
        passed,
        frames_json.join(","),
        open.join(","),
    );
}
```

This file uses `frame_{frame:04}` formatting and several `as`-free numeric paths; the only casts are inside `metrics.rs`/`diff.rs` (already allowed there). Add at the top of `capture.rs`:

```rust
#![allow(clippy::print_stdout, reason = "xtask is a CLI; printing is its job")]
```

Now wire it into the dispatcher. Edit `xtask/src/main.rs`:

```rust
mod capture;
mod check_secrets;
mod manifest;
```

Add the variant to the `Command` enum (keep the doc comment style):

```rust
#[derive(Subcommand)]
enum Command {
    /// List all xtask subcommands with descriptions.
    Manifest(manifest::Args),
    /// Regex-scan the working tree for forbidden secrets and local paths.
    CheckSecrets(check_secrets::Args),
    /// Deterministic visual capture + baseline regression for a scenario.
    Capture(capture::Args),
}
```

Add the match arm:

```rust
    let result = match cli.command {
        Command::Manifest(ref args) => {
            manifest::run(args);
            Ok(())
        }
        Command::CheckSecrets(args) => check_secrets::run(args),
        Command::Capture(args) => capture::run(args),
    };
```

Edit `xtask/src/manifest.rs` `SUBCOMMANDS` (keep the `Command` enum and this table in sync — the existing comment at lines 21–25 mandates it):

```rust
const SUBCOMMANDS: &[Entry] = &[
    Entry {
        name: "manifest",
        description: "List all xtask subcommands with descriptions.",
    },
    Entry {
        name: "check-secrets",
        description: "Regex-scan the working tree for forbidden secrets and local paths.",
    },
    Entry {
        name: "capture",
        description: "Deterministic visual capture + baseline regression for a scenario.",
    },
];
```

(If you added a temporary `mod capture { pub mod scenarios; }` in Task 5, remove it — `capture.rs` now declares `pub mod scenarios; pub mod metrics; pub mod diff;`.)

**Run-to-pass:**
`cargo test -p xtask` (all xtask tests: scenarios, metrics, diff, capture::tests)
then `cargo xtask capture --list` (should print `line-synthetic` and `line-synthetic-no-bloom`)
then `cargo xtask manifest` (should now include `capture`)
**Expected pass:** all xtask unit tests pass; `--list` prints both scenarios; manifest lists `capture`.

**Commit (operator runs later):**
`git add xtask/src/capture.rs xtask/src/main.rs xtask/src/manifest.rs && git commit -m "xtask: capture subcommand (launch+metrics+diff+report); Command enum + manifest in sync"`

---

## Task 9 — FIX `LineBoneCompositePlugin` registration + wire smear/bone-camera/force-g toggles

**Files:**
- Modify `crates/wc-sketches/src/line/mod.rs`
- Modify `crates/wc-sketches/src/line/hand_mesh.rs`
- Modify `crates/wc-sketches/src/line/audio_coupling.rs`

**Context (the fix):** `LineBoneCompositePlugin` is currently registered *inside* `LineHandMeshPlugin::build` (`hand_mesh.rs:164`). For the `WC_DEBUG_DISABLE_BONE_COMPOSITE` toggle to gate the composite node independently of the hand-mesh wiring (and to match the spec's mental model: the composite is a Line render-stage, not a hand-mesh detail), hoist it into `LinePlugin::build` and gate it there.

**Failing test** (add to the `#[cfg(test)] mod tests` in `crates/wc-sketches/src/line/mod.rs` — verifies the toggle plumbing is read; uses the pure helper, not a RenderApp):

```rust
    /// `LinePlugin` decides whether to register the bone-composite + smear
    /// nodes by reading `DebugToggles`; this guards the gating predicate.
    #[test]
    fn render_stage_gating_predicate() {
        use wc_core::debug::DebugToggles;
        let all_off = DebugToggles {
            force_g: None,
            disable_smear: false,
            disable_bloom: false,
            disable_bone_composite: false,
            disable_bone_camera: false,
            solid_particles: None,
        };
        assert!(should_register_smear(Some(&all_off)));
        assert!(should_register_bone_composite(Some(&all_off)));
        assert!(should_register_smear(None)); // no toggles → everything on
        let no_smear = DebugToggles { disable_smear: true, ..all_off };
        assert!(!should_register_smear(Some(&no_smear)));
        let no_comp = DebugToggles { disable_bone_composite: true, ..all_off };
        assert!(!should_register_bone_composite(Some(&no_comp)));
    }
```

**Run-to-fail:** `cargo test -p wc-sketches --lib line::tests::render_stage_gating_predicate`
**Expected failure:** compile error — `should_register_smear` / `should_register_bone_composite` undefined.

**Minimal implementation:**

In `crates/wc-sketches/src/line/mod.rs`, add the import and the two pure predicates (public-at-top, helpers at bottom per AGENTS.md — place predicates just above the `#[cfg(test)]` block), then change `LinePlugin::build`.

Add near the other `use wc_core::...` imports:

```rust
#[cfg(debug_assertions)]
use wc_core::debug::DebugToggles;
```

Add helpers (above the tests module):

```rust
/// Whether to register the gravity-smear post-process node. On unless
/// `WC_DEBUG_DISABLE_SMEAR` is set. Always on in release (no `DebugToggles`).
#[cfg(debug_assertions)]
fn should_register_smear(toggles: Option<&DebugToggles>) -> bool {
    !toggles.is_some_and(|t| t.disable_smear)
}

/// Whether to register the additive bone-composite node. On unless
/// `WC_DEBUG_DISABLE_BONE_COMPOSITE` is set.
#[cfg(debug_assertions)]
fn should_register_bone_composite(toggles: Option<&DebugToggles>) -> bool {
    !toggles.is_some_and(|t| t.disable_bone_composite)
}
```

Change the smear + composite registration in `LinePlugin::build`. Replace:

```rust
        // Wire the gravity-smear post-process render-graph node.
        app.add_plugins(post_process::LinePostProcessPlugin);
```

with (read `DebugToggles` once at build; `DebugPlugin` ran earlier in `CorePlugin` so the resource is present iff a toggle was set):

```rust
        // Wire the gravity-smear post-process render-graph node. In debug
        // builds, `WC_DEBUG_DISABLE_SMEAR` skips it for render-stage isolation.
        #[cfg(debug_assertions)]
        let toggles = app.world().get_resource::<DebugToggles>().copied();
        #[cfg(debug_assertions)]
        let register_smear = should_register_smear(toggles.as_ref());
        #[cfg(not(debug_assertions))]
        let register_smear = true;
        if register_smear {
            app.add_plugins(post_process::LinePostProcessPlugin);
        }

        // Wire the additive bone-glow composite node here (hoisted out of
        // `LineHandMeshPlugin` so `WC_DEBUG_DISABLE_BONE_COMPOSITE` can gate it
        // at the Line render-stage level). The composite reads `HandMeshTarget`
        // (extracted from the main world) and no-ops cleanly when that resource
        // is absent, so registering it independently of the bone camera is safe.
        #[cfg(debug_assertions)]
        let register_bone_composite = should_register_bone_composite(toggles.as_ref());
        #[cfg(not(debug_assertions))]
        let register_bone_composite = true;
        if register_bone_composite {
            app.add_plugins(bone_composite::LineBoneCompositePlugin);
        }
```

Add the `use` for `bone_composite` at the top of `mod.rs` if not already imported via the `pub mod bone_composite;` path — reference it as `bone_composite::LineBoneCompositePlugin` (the module is already declared at line 31).

In `crates/wc-sketches/src/line/hand_mesh.rs`, remove the composite registration (now owned by `LinePlugin`) and gate the bone camera spawn. Change the import line 101:

```rust
use super::bone_composite::HandMeshTarget;
```

(drop `LineBoneCompositePlugin` from the import). Change `LineHandMeshPlugin::build` — remove the `.add_plugins(LineBoneCompositePlugin)` line and gate the camera spawn:

```rust
impl Plugin for LineHandMeshPlugin {
    fn build(&self, app: &mut App) {
        // NOTE: `LineBoneCompositePlugin` is registered by `LinePlugin::build`
        // (so `WC_DEBUG_DISABLE_BONE_COMPOSITE` can gate the composite node at
        // the Line render-stage level), not here.
        app.add_plugins(MaterialPlugin::<BoneWireframeMaterial>::default());

        // In debug builds, `WC_DEBUG_DISABLE_BONE_CAMERA` skips spawning the
        // off-screen bone camera for render-stage isolation.
        #[cfg(debug_assertions)]
        let spawn_camera = !app
            .world()
            .get_resource::<wc_core::debug::DebugToggles>()
            .is_some_and(|t| t.disable_bone_camera);
        #[cfg(not(debug_assertions))]
        let spawn_camera = true;
        if spawn_camera {
            app.add_systems(OnEnter(AppState::Line), spawn_hand_mesh_camera);
        }

        app.add_systems(
            OnExit(AppState::Line),
            (despawn_hand_mesh_camera, despawn_all_bone_children),
        )
        .add_systems(
            Update,
            (ensure_bone_meshes, update_bone_transforms)
                .chain()
                .run_if(sketch_active(AppState::Line)),
        )
        .add_systems(Update, resize_bone_target.run_if(in_state(AppState::Line)));
    }
}
```

In `crates/wc-sketches/src/line/audio_coupling.rs`, honour `force_g` in `drive_audio_and_shader`. Add the param (debug-only) and override after the triangle-wave line. Change the `drive_audio_and_shader` signature to take an optional `DebugToggles`:

```rust
#[cfg(debug_assertions)]
    debug_toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
```

and after the existing line `post.g_constant = triangle_wave_approx(...) * ...;` (line 168), add:

```rust
    // WC_DEBUG_FORCE_G pins the gravity constant, eliminating the triangle-wave
    // phase variable for deterministic render-stage isolation (debug only).
    #[cfg(debug_assertions)]
    if let Some(forced) = debug_toggles.as_ref().and_then(|t| t.force_g) {
        post.g_constant = forced;
    }
```

(Place `debug_toggles` as the last system param so the non-debug build's signature is unaffected.)

**Run-to-pass:**
`cargo test -p wc-sketches --lib line::tests::render_stage_gating_predicate`
then `cargo build -p waveconductor` (confirms the hoisted composite + gated camera + force-g still compile against the real RenderApp).
**Expected pass:** predicate test passes; debug build compiles.

**Commit (operator runs later):**
`git add crates/wc-sketches/src/line/mod.rs crates/wc-sketches/src/line/hand_mesh.rs crates/wc-sketches/src/line/audio_coupling.rs && git commit -m "line: hoist LineBoneCompositePlugin to LinePlugin; wire DISABLE_SMEAR/BONE_CAMERA/FORCE_G toggles"`

---

## Task 10 — Solid-particle override (material uniform + shader) and bloom toggle

**Files:**
- Modify `crates/wc-sketches/src/line/material.rs`
- Modify `crates/wc-sketches/src/line/systems/spawn.rs`
- Modify `assets/shaders/line/render.wgsl`
- Modify `crates/waveconductor/src/main.rs`

**Failing test** (add to `crates/wc-sketches/src/line/material.rs` footer):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_solid_color_is_off() {
        // alpha == 0 means "off" (use the star texture). Constructed via the
        // helper so spawn.rs and tests agree on the off-sentinel.
        assert_eq!(LineMaterial::solid_off(), Vec4::ZERO);
    }
}
```

**Run-to-fail:** `cargo test -p wc-sketches --lib line::material`
**Expected failure:** compile error — `solid_off` and the `solid_color` field don't exist.

**Minimal implementation:**

Edit `crates/wc-sketches/src/line/material.rs`. Add the field + helper:

```rust
#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct LineMaterial {
    /// Particle storage buffer, read-only from the vertex shader.
    #[storage(0, read_only)]
    pub particles: Handle<ShaderStorageBuffer>,
    /// Star sprite texture sampled in the fragment shader. The texture's
    /// alpha modulates each particle's final alpha so quads render as soft
    /// star points instead of flat-color rectangles.
    #[texture(1)]
    #[sampler(2)]
    pub star_texture: Handle<Image>,
    /// Debug solid-particle override (linear RGBA). When `a > 0` the fragment
    /// shader returns this flat colour instead of the star texel — the
    /// "magenta isolation" trick (`WC_DEBUG_SOLID_PARTICLES`). `Vec4::ZERO`
    /// (the [`Self::solid_off`] sentinel) means "off".
    #[uniform(3)]
    pub solid_color: Vec4,
}

impl LineMaterial {
    /// The `solid_color` sentinel meaning "off" (use the star texture).
    pub fn solid_off() -> Vec4 {
        Vec4::ZERO
    }
}
```

Edit `crates/wc-sketches/src/line/systems/spawn.rs`. Where `LineMaterial { particles, star_texture }` is constructed (the spawn site that builds the material around line 145–173), seed `solid_color`. Add a `DebugToggles` system param (debug-only) and read it:

```rust
    #[cfg(debug_assertions)] debug_toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
```

Compute the colour before constructing the material:

```rust
    #[cfg(debug_assertions)]
    let solid_color = debug_toggles
        .as_ref()
        .and_then(|t| t.solid_particles)
        .map_or_else(LineMaterial::solid_off, |[r, g, b, a]| Vec4::new(r, g, b, a));
    #[cfg(not(debug_assertions))]
    let solid_color = LineMaterial::solid_off();
```

and add `solid_color` to the `LineMaterial { … }` struct literal.

Edit `assets/shaders/line/render.wgsl`. Add the uniform binding (group 2, binding 3 — the next free slot) and branch in the fragment:

```wgsl
@group(2) @binding(0) var<storage, read> particles: array<Particle>;
@group(2) @binding(1) var star_texture: texture_2d<f32>;
@group(2) @binding(2) var star_sampler: sampler;
// Debug solid-particle override (linear RGBA). a > 0 => return the flat colour
// instead of the star texel. Set from `LineMaterial.solid_color`
// (WC_DEBUG_SOLID_PARTICLES). Vec4(0) means "off" in normal runs and release.
@group(2) @binding(3) var<uniform> solid_color: vec4<f32>;
```

and replace the fragment body:

```wgsl
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Debug isolation: when a solid override colour is set (alpha > 0), render
    // every particle as that flat colour, modulated only by per-particle alpha.
    // Separates particle geometry from the star texture / smear contribution.
    if (solid_color.a > 0.0) {
        return vec4<f32>(solid_color.rgb, solid_color.a * in.alpha);
    }
    let texel = textureSample(star_texture, star_sampler, in.uv);
    // v4 uses THREE.PointsMaterial with vertexColors:true and a vertex color
    // of (1, 1, 1). The texture RGB is multiplied by the vertex color, which
    // is a no-op — the star sprite's own RGB (near-white at centre) is used
    // directly. Velocity-based dimming was NOT present in v4 and caused
    // stationary particles to render at 5% brightness instead of the correct
    // ~89% (the star.png centre pixel is RGBA(228,221,222,237)).
    // Final alpha = sprite-alpha × particle-alpha so quad corners fade smoothly.
    return vec4<f32>(texel.rgb, texel.a * in.alpha);
}
```

Edit `crates/waveconductor/src/main.rs` to add the bloom toggle. Add (debug-only) a system that zeroes/restores `Bloom.intensity` from `DebugToggles`. After the existing `spawn_camera` is registered, add a debug-only `Update` system. Add near the top:

```rust
#[cfg(debug_assertions)]
use bevy::post_process::bloom::Bloom;
```

Add the system (debug-only), placed after `spawn_camera`:

```rust
/// Apply `WC_DEBUG_DISABLE_BLOOM`: zero the main camera's bloom intensity for
/// render-stage isolation (debug builds only). Runs once on the first frame the
/// camera exists; cheap to re-run (only mutates on the change tick). The
/// override never restores a non-default value because nothing else writes
/// bloom intensity at runtime in this app.
#[cfg(debug_assertions)]
fn apply_debug_bloom_toggle(
    toggles: Option<Res<'_, wc_core::debug::DebugToggles>>,
    mut query: Query<'_, '_, &mut Bloom, With<Camera2d>>,
) {
    let Some(toggles) = toggles else {
        return;
    };
    if !toggles.disable_bloom {
        return;
    }
    for mut bloom in &mut query {
        if bloom.intensity != 0.0 {
            bloom.intensity = 0.0;
        }
    }
}
```

Register it (debug-only) in the `Startup`/`Update` wiring. After the `.add_systems(Startup, (...))` block, add:

```rust
    #[cfg(debug_assertions)]
    app.add_systems(Update, apply_debug_bloom_toggle);
```

**Run-to-pass:**
`cargo test -p wc-sketches --lib line::material`
then `cargo build -p waveconductor`
**Expected pass:** material test passes; the binary (with the WGSL binding-3 addition and the bloom toggle) compiles.

**Commit (operator runs later):**
`git add crates/wc-sketches/src/line/material.rs crates/wc-sketches/src/line/systems/spawn.rs assets/shaders/line/render.wgsl crates/waveconductor/src/main.rs && git commit -m "line: SOLID_PARTICLES material uniform + shader branch; DISABLE_BLOOM toggle"`

---

## Task 11 — Release-profile guard comment + baseline dir scaffold

**Files:**
- Modify `Cargo.toml`
- Create `tests/visual/baselines/line-synthetic/.gitkeep`

**Failing test:** (No code under test — this is config + a tracked empty dir. The "verification" is a grep, run-to-fail below.)

**Run-to-fail:**
`grep -q "debug-assertions" Cargo.toml; echo "exit=$?"` (expect `exit=1` — the guard comment is absent)
and `test -f tests/visual/baselines/line-synthetic/.gitkeep; echo "exit=$?"` (expect `exit=1`)

**Minimal implementation:**

Edit `Cargo.toml` `[profile.release]` — append the guard comment (do NOT set `debug-assertions = true`; the guard explains why):

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
# GUARD: do NOT set `debug-assertions = true` here (or on the soak profile).
# The wc-capture visual-debugging scaffold (`wc_core::capture`,
# `wc_core::debug`, and their CorePlugin registration) is compiled out via
# `#[cfg(debug_assertions)]`. Release defaults to `debug-assertions = false`,
# which is exactly what keeps the capture/debug systems and their per-frame work
# out of release/soak builds. Enabling assertions here would compile them back
# in. See crates/wc-core/src/capture/mod.rs and crates/wc-core/src/debug/mod.rs.
```

Create the tracked-but-empty baseline directory:

```bash
mkdir -p tests/visual/baselines/line-synthetic
: > tests/visual/baselines/line-synthetic/.gitkeep
```

`.gitkeep` content (one line is fine):

```
# Baselines for the line-synthetic scenario land here via
# `cargo xtask capture line-synthetic --update-baselines` (plain committed PNGs).
```

**Run-to-pass:**
`grep -q "debug-assertions" Cargo.toml; echo "exit=$?"` (expect `exit=0`)
and `test -f tests/visual/baselines/line-synthetic/.gitkeep; echo "exit=$?"` (expect `exit=0`)
then `cargo xtask check-secrets` (the scaffold introduces no home paths/secrets; `tests/` and `docs/` are already skipped by check-secrets).
**Expected pass:** both greps exit 0; check-secrets reports 0 findings.

**Commit (operator runs later):**
`git add Cargo.toml tests/visual/baselines/line-synthetic/.gitkeep && git commit -m "Cargo: guard release debug-assertions; scaffold baseline dir"`

---

## Task 12 — Harness docs (`tests/visual/CLAUDE.md`)

**Files:**
- Create `tests/visual/CLAUDE.md`

**Failing test:** (Documentation; verified by presence + content grep.)

**Run-to-fail:**
`test -f tests/visual/CLAUDE.md; echo "exit=$?"` (expect `exit=1`)

**Minimal implementation** (full file `tests/visual/CLAUDE.md`):

````markdown
# Visual-capture harness (wc-capture)

Deterministic, agent-driven frame capture + visual regression for WaveConductor
sketches. The app self-captures via Bevy's screenshot API; `cargo xtask capture`
orchestrates the launch and does all image work via the `image` crate. No LLM /
vision API spend: the operating agent is the visual judge (it Reads the flagged
PNGs itself).

## Quick start

```bash
cargo xtask capture --list                 # list scenarios
cargo xtask capture line-synthetic         # capture + diff baselines (human table)
cargo xtask capture line-synthetic --json  # machine output (names frames to open)
cargo xtask capture line-synthetic --update-baselines   # adopt current frames as baseline
cargo xtask capture line-synthetic --watch=10           # live demo, no capture, quit after 10s
cargo xtask capture line-synthetic --debug FORCE_G=4000 --debug DISABLE_BLOOM=1
```

Output lands under `target/capture/<scenario>/`:
`frame_NNNN.png`, `run.json` (app-written sidecar), `metrics.json`, `app.log`,
`clean-config/` (fresh settings dir for `config = "clean"`).

Exit code: `0` pass (and for `--watch` / `--update-baselines`); nonzero when a
frame regresses beyond tolerance.

## Scenarios (`tests/visual/scenarios.toml`)

```toml
[scenarios.<name>]
sketch   = "line"          # -> WAVECONDUCTOR_START_SKETCH
provider = "synthetic"     # -> WAVECONDUCTOR_HAND_PROVIDER (synthetic|mock|leap|auto)
config   = "clean"         # "clean" = fresh temp config dir; else a pinned path
frames   = [30, 60, 120]   # sim-frame indices to capture (frame 0 = first fully-loaded, settled frame)
dt       = 0.016666667     # optional fixed timestep (default 1/60 in the app)

[scenarios.<name>.debug]   # optional WC_DEBUG_* toggles (KEY without the prefix)
FORCE_G       = "8000"
DISABLE_BLOOM = "1"
```

Add a scenario: append a `[scenarios.<name>]` table; capture once with
`--update-baselines` to seed `tests/visual/baselines/<name>/`.

## `WC_DEBUG_*` render-stage isolation toggles

Parsed once at startup into `wc_core::debug::DebugToggles` (debug builds only;
compiled out of release). Set any to activate the resource.

| Var | Effect |
|-----|--------|
| `WC_DEBUG_FORCE_G=<f32>` | Pin the Line gravity-smear `g_constant` (removes the triangle-wave phase variable). |
| `WC_DEBUG_DISABLE_SMEAR` | Skip the gravity post-process node. |
| `WC_DEBUG_DISABLE_BLOOM` | Zero the main camera bloom. |
| `WC_DEBUG_DISABLE_BONE_COMPOSITE` | Skip the additive bone-composite node. |
| `WC_DEBUG_DISABLE_BONE_CAMERA` | Do not spawn the off-screen bone camera. |
| `WC_DEBUG_SOLID_PARTICLES=<rgba hex>` | Render particles as a flat colour (6 or 8 hex digits). |

## `WC_CAPTURE` contract (set by xtask; documented for reference)

`WC_CAPTURE="dir=<path>;frames=<n,n,...>[;dt=<secs>][;settle=<n>]"`
- `dir`: output dir for `frame_NNNN.png` + `run.json`.
- `frames`: sim-frame indices to screenshot. Frame 0 = first fully-loaded sketch
  frame (after assets-ready + `settle`).
- `dt`: fixed timestep, default `1/60`. `settle`: frames to wait after
  assets-ready, default `2`.

## `--json` shape

```json
{
  "scenario": "line-synthetic",
  "dir": "target/capture/line-synthetic",
  "passed": true,
  "frames": [
    {
      "frame": 30,
      "full_mean": [r, g, b], "center_mean": [r, g, b],
      "global_std": 0.0,
      "mean_abs_diff": 0.0,   // null when no baseline yet
      "passed": true,
      "current": "target/capture/line-synthetic/frame_0030.png",
      "baseline": "tests/visual/baselines/line-synthetic/frame_0030.png"  // or null
    }
  ],
  "open_for_review": ["...png", "..."]   // frames the agent should open & judge
}
```

## When to update baselines

Baselines are environment-sensitive: GPU/driver float differences make exact
matching brittle, so the diff is tolerance-based and you (the agent) review
flagged frames. Re-baseline (`--update-baselines`) only on the deployment-class
machine, and only after visually confirming the new frames are correct. Commit
the PNGs (plain, no Git LFS).

## Determinism + headless note

Capture needs a real render surface (macOS dev has a display). The round-trip
smoke check is a dev-machine task, not headless CI. Fixed-dt pins the visual sim
(particles, smear, synthetic-hand sweep, `g_constant`); the audio thread does
not affect captured visuals.
````

**Run-to-pass:**
`test -f tests/visual/CLAUDE.md && grep -q "WC_DEBUG_SOLID_PARTICLES" tests/visual/CLAUDE.md; echo "exit=$?"` (expect `exit=0`)

**Commit (operator runs later):**
`git add tests/visual/CLAUDE.md && git commit -m "docs: wc-capture harness CLAUDE.md (scenarios, flags, JSON shape, toggles)"`

---

## Task 13 — Real smoke capture + round-trip baseline (dev machine, display required)

This is the end-to-end verification: launch the real debug binary under a
scenario, confirm the app self-captures deterministic PNGs, seed baselines, then
re-run to prove the diff round-trips to "pass." Requires a display (macOS dev).

**Files:** none created — exercises everything end to end. Produces
`target/capture/line-synthetic/frame_*.png` and seeds
`tests/visual/baselines/line-synthetic/frame_*.png`.

**Failing test (run-to-fail):**
`cargo xtask capture line-synthetic --json`
**Expected failure:** nonzero exit is NOT expected here on the *first* run — instead, every frame reports `"baseline": null` / verdict `NEW (review)` because no baselines exist yet (exit 0, but `open_for_review` lists all frames). That "all frames NEW" state is the pre-baseline signal. Concretely, before this task there are no `frame_*.png` under `tests/visual/baselines/line-synthetic/`, so the diff has nothing to compare against.

**Minimal implementation / procedure:**

1. Build + run the capture once:
   ```bash
   cargo xtask capture line-synthetic --json
   ```
   Confirm `target/capture/line-synthetic/` now contains
   `frame_0030.png frame_0060.png frame_0120.png frame_0240.png run.json metrics.json app.log`.
   Read `app.log` to confirm `WC_CAPTURE active`, `capture: requesting screenshot`
   (×4), `capture: wrote run.json`, and `capture: schedule complete, requesting AppExit`.

2. **Agent visual judgment (no API spend):** Read the four PNGs. Confirm the
   Line sketch rendered with the synthetic open hand (bone glow present, gravity
   smear present, particles visible). If a frame looks wrong, do NOT baseline —
   diagnose with isolation toggles, e.g.:
   ```bash
   cargo xtask capture line-synthetic --debug DISABLE_BLOOM=1 --json
   cargo xtask capture line-synthetic --debug SOLID_PARTICLES=ff00ff --json
   cargo xtask capture line-synthetic --debug FORCE_G=8000 --json
   ```

3. Once the frames are confirmed correct, seed baselines:
   ```bash
   cargo xtask capture line-synthetic --update-baselines
   ```
   Confirm `tests/visual/baselines/line-synthetic/frame_*.png` now exist.

4. **Round-trip proof:** re-run and confirm the diff passes:
   ```bash
   cargo xtask capture line-synthetic --json
   ```

**Run-to-pass:**
`cargo xtask capture line-synthetic --json`
**Expected pass:** exit 0; JSON `"passed": true`; every frame `"passed": true`
with a finite small `mean_abs_diff` (identical capture vs its own baseline → ~0,
within `DIFF_TOLERANCE`); `open_for_review` empty.

**Commit (operator runs later):**
`git add tests/visual/baselines/line-synthetic/frame_0030.png tests/visual/baselines/line-synthetic/frame_0060.png tests/visual/baselines/line-synthetic/frame_0120.png tests/visual/baselines/line-synthetic/frame_0240.png && git commit -m "tests/visual: seed line-synthetic baselines (plain PNGs)"`

---

## Final verification checklist (operator)

- `cargo test -p wc-core` (capture::config, capture::system, debug, lib gating).
- `cargo test -p xtask` (scenarios, metrics, diff, capture::tests).
- `cargo test -p wc-sketches --lib line::` (gating predicate, material).
- `cargo build -p waveconductor` (debug — full render path compiles with toggles).
- `cargo build -p waveconductor --release` (confirms capture/debug compile OUT;
  no `wc_core::capture` / `wc_core::debug` symbols).
- `cargo xtask manifest` lists `capture`; `cargo xtask capture --list` lists both scenarios.
- `cargo xtask check-secrets` reports 0 findings.
- `cargo clippy --workspace --all-targets` clean (the only `as` casts are the
  scoped-allowed image-stat averages in `metrics.rs` / `diff.rs`).
- Task 13 round-trip: `cargo xtask capture line-synthetic --json` exits 0,
  `"passed": true`.

**Do not commit during planning execution unless the operator asks.** Each task
lists the commit the operator runs later; leave the working tree dirty for review.

