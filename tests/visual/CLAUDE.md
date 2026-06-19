# Visual-capture harness (wc-capture)

Deterministic, agent-driven frame capture + visual regression for WaveConductor
sketches. The app self-captures via Bevy's screenshot API; `cargo xtask capture`
orchestrates the launch and does all image work via the `image` crate. No LLM /
vision API spend: the operating agent is the visual judge — it Reads the flagged
PNGs itself and applies judgment (the harness only computes cheap metrics that
say *which* frames to open).

The whole scaffold (`wc_core::capture`, `wc_core::debug`, and their render
wiring) is `#[cfg(debug_assertions)]`-gated and compiled out of release. Capture
runs against the **debug** binary (`cargo run -p waveconductor`).

## Quick start

```bash
cargo xtask capture --list                 # list scenarios (human)
cargo xtask capture --list --json          # list scenarios as a JSON array of names
cargo xtask capture line-synthetic         # capture + diff baselines (human table)
cargo xtask capture line-synthetic --json  # machine output (names frames to open)
cargo xtask capture line-synthetic --update-baselines     # adopt current frames as baseline
cargo xtask capture line-synthetic --watch=10             # live, no capture, quit after 10s
cargo xtask capture line-synthetic --debug FORCE_G=4000 --debug DISABLE_BLOOM=1
```

`cargo xtask capture` is a subcommand of the xtask dispatcher; `cargo xtask
manifest` lists it alongside the other subcommands.

## Flags (`cargo xtask capture`)

| Flag | Effect |
|------|--------|
| `<scenario>` (positional) | Scenario name from `scenarios.toml`. Required unless `--list`. |
| `--list` | List available scenario names and exit (no launch). Honours `--json`. |
| `--json` | Emit machine-readable JSON instead of the human table. Also reshapes `--list` and `--update-baselines` output. |
| `--update-baselines` | Launch + capture, then copy the fresh frames into `baselines/<scenario>/`. No diff gate; always exits 0. Use only after visual confirmation. |
| `--watch[=SECS]` | Launch the scenario for hands-on inspection (no capture, normal variable-dt clock), then kill after `SECS` (default `10`). |
| `--debug KEY=VAL` | Ad-hoc `WC_DEBUG_*` override (KEY *without* the `WC_DEBUG_` prefix). Repeatable. Merges over the scenario's `debug` table; CLI wins. |

Output lands under `target/capture/<scenario>/`:
`frame_NNNN.png` (one per scheduled frame, e.g. `frame_0030.png`), `run.json`
(app-written, self-describing sidecar: `scenario`, scheduled `frames`, `dt_secs`,
`settle`, `app_version`, `commit` (short git hash), and the active `toggles`
object — enough to reproduce the run; `scenario`/`commit` are `null` if the
launcher didn't supply them), `metrics.json` (per-frame metrics array),
`app.log` (teed stdout+stderr), `clean-config/` (fresh settings dir when
`config = "clean"`).

Exit code: `0` on pass (and for `--list` / `--watch` / `--update-baselines`);
nonzero when a frame regresses beyond tolerance. Tolerance is mean-abs-diff
`<= 6.0` (0..=255 units); a pixel counts as "changed" when its max-channel delta
exceeds `12`. A frame with no baseline yet never regresses — it is reported as
`NEW (review)` and added to `open_for_review`.

## Scenarios (`scenarios.toml`)

Scenarios defined today:

| Name | sketch | provider | config | frames | debug |
|------|--------|----------|--------|--------|-------|
| `line-synthetic` | `line` | `synthetic` | `clean` | `[30, 60, 120, 240]` | — |
| `line-synthetic-no-bloom` | `line` | `synthetic` | `clean` | `[30, 60, 120, 240]` | `DISABLE_BLOOM = "1"` |
| `line-screensaver` | `line` | `mock` | `clean` | `[180, 276, 495, 570, 666, 780, 1320, 1770]` | `FORCE_SCREENSAVER` |

The `line-screensaver` scenario drives Line's attract mode: the "Wandering
Pulses" choreography
(`wc-sketches/src/line/screensaver/choreography.rs`) — three slow Lissajous
walkers, each briefly pulsing gentle attraction (peak 0.35, 1.2 s on, once
per 14 / 19 / 23.5 s) — plus a gentle divergence-free noise turbulence
(`attract_turbulence`) that slowly morphs the whole field, which is the
screensaver's primary motion. (An earlier composition also had fast "meteor"
attractors; they were cut for jolting the gravity smear too hard.) A
fade-ramped brightness lift (`attract_brightness`) keeps the calm field's
whites bright under the `AgX` tonemapper instead of dim grey. Attract mode also
thins the field to `attract_particle_fraction` (default 0.6) and respawns each
surviving particle at its spawn position on a staggered ~10–18 s lifetime,
so the picture continuously self-heals. It forces
`SketchActivity::Screensaver` at startup; the choreography is thermal-tier
agnostic, so one scenario covers all tiers (the tier only changes present
rate, which the capture clock ignores). The frame spread samples a spread of
the pulse schedule + the steadily-building turbulence drift (indices documented
in `scenarios.toml`).
Expected per-frame signal: `delta_prev` ~10–30 (continuous gentle motion —
never ~0/frozen, never the old grab's mass convergence). Review the PNGs to
confirm: (a) visibly Line — the particle line spans the frame and stays
readable in *every* frame (thinner than Active, by design), (b) the field
slowly morphs into a gentle organic undulation (turbulence), never tangling
into a knot, (c) pulses read as a gentle local bow/wave near the walker, not a
collapse toward it, (d) whites stay bright white (not dim grey), and (e)
stirred-up particles pick up a subtle cool tint (velocity color, attract-only),
while calm regions keep the warm-white personality.

Schema:

```toml
[scenarios.<name>]
sketch   = "line"          # -> WAVECONDUCTOR_START_SKETCH (line|flame|dots|cymatics|waves)
provider = "synthetic"     # -> WAVECONDUCTOR_HAND_PROVIDER (synthetic|mock|leap|mediapipe|auto|off — launch default)
config   = "clean"         # "clean" = fresh temp config dir; any other value is a path pinned via WAVECONDUCTOR_CONFIG_DIR
frames   = [30, 60, 120]   # sim-frame indices to capture (frame 0 = first fully-loaded, settled frame)
dt       = 0.016666667     # optional fixed timestep in seconds (default 1/60 in the app)

[scenarios.<name>.debug]   # optional WC_DEBUG_* toggles (KEY without the WC_DEBUG_ prefix)
FORCE_G       = "8000"
DISABLE_BLOOM = "1"
```

`provider = "synthetic"` emits a stationary synthetic open hand (deterministic
hand visuals without hardware). `mock` is the silent empty mock, `leap` requires
hardware, `auto` (default) tries Leap then falls back to mock.

### Adding a scenario

1. Append a `[scenarios.<name>]` table to `scenarios.toml` (and an optional
   `[scenarios.<name>.debug]` sub-table). Keep `config = "clean"` unless you
   need to pin a specific on-disk settings dir.
2. Capture once and visually confirm the frames look correct.
3. Seed baselines: `cargo xtask capture <name> --update-baselines`. This writes
   `baselines/<name>/frame_NNNN.png` (plain committed PNGs, no Git LFS).
4. Commit `scenarios.toml` and the new baseline PNGs.

## `WC_DEBUG_*` render-stage isolation toggles

Parsed once at startup into `wc_core::debug::DebugToggles` (debug builds only).
The resource is inserted *only* when at least one `WC_DEBUG_*` var is present;
otherwise every consumer treats the absent resource as "all toggles off". Set via
the scenario's `[debug]` table or `--debug KEY=VAL` (the launcher re-prefixes
`KEY` to `WC_DEBUG_<KEY>`).

| Var | Effect |
|-----|--------|
| `WC_DEBUG_FORCE_G=<f32>` | Pin the Line gravity-smear `g_constant`, eliminating the triangle-wave phase variable (deterministic isolation). Bad float -> ignored. |
| `WC_DEBUG_DISABLE_SMEAR` | Skip the gravity post-process node (presence = on; value ignored). |
| `WC_DEBUG_DISABLE_BLOOM` | Zero the main camera bloom intensity. |
| `WC_DEBUG_DISABLE_BONE_COMPOSITE` | Skip the additive bone-glow composite node. |
| `WC_DEBUG_DISABLE_BONE_CAMERA` | Do not spawn the off-screen bone camera. |
| `WC_DEBUG_SOLID_PARTICLES=<rgba hex>` | Render particles as a flat linear colour instead of the star texel. 6 hex digits (alpha defaults to `ff`) or 8 (`rrggbbaa`), no `#`. Bad/odd-length hex -> ignored. |
| `WC_DEBUG_FORCE_SCREENSAVER` | Drive `SketchActivity::Screensaver` at startup so a capture lands in attract mode without waiting out the idle timer (presence = on; value ignored). |
| `WC_DEBUG_FORCE_TIER=<cool\|warm\|hot>` | Pin the screensaver's thermal tier so each tier can be captured deterministically. Unparseable value -> live `ThermalState`. |

Flag toggles (`DISABLE_*`) are true whenever their var is present, regardless of
value — `=1` and `=` both activate them. `FORCE_G` and `SOLID_PARTICLES` are
value-typed and silently ignore unparseable input.

## `WC_CAPTURE` env format (set by xtask; documented for reference)

`WC_CAPTURE="dir=<path>;frames=<n,n,...>[;dt=<secs>][;settle=<n>][;scenario=<name>][;commit=<hash>]"`

- `dir` (required): output dir for `frame_NNNN.png` + `run.json`.
- `frames` (required, >=1): sim-frame indices to screenshot. Frame 0 = first
  fully-loaded sketch frame (after assets-ready + `settle`). Sorted + deduped.
- `dt` (optional, seconds): fixed virtual-time delta. Default `1/60`
  (`16_666_667` ns).
- `settle` (optional): frames to wait after assets-ready before frame 0.
  Default `2`.
- `scenario` (optional): scenario name, recorded verbatim in `run.json`. The
  xtask sets it; not meaningful to pass by hand.
- `commit` (optional): short git commit hash, recorded in `run.json`. The xtask
  resolves it via `git rev-parse --short HEAD`; absent outside a repo.

The capture system pins `Time<Virtual>` to `dt` once the sketch's assets are
ready, screenshots the scheduled frames, writes `run.json`, and requests
`AppExit` after the last frame. `--watch` launches *without* `WC_CAPTURE`, so the
app runs normally and is killed by the wall-clock.

## `--json` shape

```json
{
  "scenario": "line-synthetic",
  "dir": "target/capture/line-synthetic",
  "passed": true,
  "frames": [
    {
      "frame": 30,
      "full_mean": [r, g, b],
      "center_mean": [r, g, b],
      "global_std": 0.0,
      "mean_abs_diff": 0.0,
      "passed": true,
      "current": "target/capture/line-synthetic/frame_0030.png",
      "baseline": "tests/visual/baselines/line-synthetic/frame_0030.png"
    }
  ],
  "open_for_review": ["...png", "..."]
}
```

Notes:
- `mean_abs_diff` is `null` when there is no baseline yet; `baseline` is `null`
  in the same case.
- `passed` (top-level) is the AND of every frame's `passed`. A frame with no
  baseline counts as passed but still appears in `open_for_review`.
- `open_for_review` lists the frame `current` paths the agent should open and
  judge (regressions + new-baseline frames). Read those PNGs directly.
- `--list --json` prints a bare JSON array of scenario names, e.g.
  `["line-synthetic","line-synthetic-no-bloom"]`.
- `--update-baselines --json` prints
  `{"scenario":"<name>","updated_baselines":true}`.

The per-frame `metrics.json` sidecar (always written) is a JSON array of
`{frame, full_mean, center_mean, global_std, delta_prev}`, where `delta_prev` is
the mean-abs per-channel delta vs the previous captured frame (`null` for the
first) — the frozen-vs-animated signal.

## When (and how) to update baselines

Baselines are GPU/environment-sensitive: driver float differences make
pixel-exact matching brittle, which is why the diff is tolerance-based and you
(the agent) review flagged frames rather than trusting a hard equality gate.

- Re-baseline (`--update-baselines`) only on the **deployment-class machine**,
  and only after Reading the new frames and visually confirming they are
  correct. Baselines captured on a different GPU/driver will systematically
  drift against another machine.
- If a frame looks wrong, do **not** baseline. Diagnose with isolation toggles
  first, e.g. `--debug DISABLE_BLOOM=1`, `--debug SOLID_PARTICLES=ff00ff`,
  `--debug DISABLE_SMEAR=1`, then fix the code and re-capture.
- Commit baseline PNGs as plain files (no Git LFS).

## Determinism + headless note

Capture needs a real render surface (macOS dev has a display); the round-trip
smoke check is a dev-machine task, not headless CI. Fixed-`dt` pins the visual
sim (particles, smear, synthetic-hand sweep, `g_constant`); the audio thread does
not affect captured visuals. The xtask enforces a 90s wall-clock timeout on the
launched app as a safety net in case a screenshot observer never fires (then it
errors and points you at `app.log`).
