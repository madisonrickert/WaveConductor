# Visual-capture harness (wc-capture)

Deterministic, agent-driven frame capture + visual regression for WaveConductor
sketches. The app self-captures via Bevy's screenshot API; `cargo xtask capture`
orchestrates the launch and does all image work via the `image` crate. No LLM /
vision API spend: the operating agent is the visual judge — it Reads the flagged
PNGs itself and applies judgment (the harness only computes cheap metrics that
say *which* frames to open).

The whole scaffold (`wc_core::capture`, `wc_core::debug`, and their render
wiring) is `#[cfg(debug_assertions)]`-gated and compiled out of release. Capture
runs against the **pre-built debug** binary (`target/debug/waveconductor`).

**Build the app first** — capture does *not* build it. Run `cargo build -p
waveconductor` as a separate, watchable step, then run a capture. If the binary
is missing, `cargo xtask capture` fails fast with that exact directive rather
than building under its launch timeout (a cold build would otherwise trip the
90s "app did not exit" safety net mid-compile — capture would look like it hung
when it was really still compiling). Capture also prints a non-fatal warning if
the built binary is older than `crates/` or `assets/shaders/` sources; rebuild
to refresh.

## Quick start

```bash
cargo build -p waveconductor               # REQUIRED first: capture launches this pre-built binary
cargo xtask capture --list                 # list scenarios (human)
cargo xtask capture --list --json          # list scenarios as a JSON array of names
cargo xtask capture line-synthetic         # capture + diff baselines (human table)
cargo xtask capture line-synthetic --json  # machine output (names frames to open)
cargo xtask capture line-synthetic --update-baselines     # adopt current frames as baseline
cargo xtask capture line-synthetic --watch=10             # live, no capture, quit after 10s
cargo xtask capture line-synthetic --debug FORCE_G=4000 --debug DISABLE_BLOOM=1
cargo xtask capture line-synthetic --update-baselines --allow-black  # only if black IS correct
```

`cargo xtask capture` is a subcommand of the xtask dispatcher; `cargo xtask
manifest` lists it alongside the other subcommands.

## Flags (`cargo xtask capture`)

| Flag | Effect |
|------|--------|
| `<scenario>` (positional) | Scenario name from `scenarios.toml`. Required unless `--list`. |
| `--list` | List available scenario names and exit (no launch). Honours `--json`. |
| `--json` | Emit machine-readable JSON instead of the human table. Also reshapes `--list` and `--update-baselines` output. |
| `--update-baselines` | Launch + capture, then copy the fresh frames into `baselines/<scenario>/`. No tolerance diff gate, but *is* gated by the near-zero-luminance ("all-black") guard below — refuses and exits nonzero if any captured frame is effectively black. Use only after visual confirmation. |
| `--allow-black` | Only meaningful with `--update-baselines`: lets it bless a batch containing near-zero-luminance frames. Only pass this when black is genuinely the correct rendered output — see the black-frame environment trap below before reaching for it. |
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

Exit code: `0` on pass (and for `--list` / `--watch` / a successful
`--update-baselines`); nonzero when a frame regresses beyond tolerance, or when
`--update-baselines` is refused by the near-zero-luminance guard (see below).
Tolerance is mean-abs-diff `<= 6.0` (0..=255 units); a pixel counts as
"changed" when its max-channel delta exceeds `12`. A frame with no baseline
yet never regresses — it is reported as `NEW (review)` and added to
`open_for_review`.

## Scenarios (`scenarios.toml`)

Scenarios defined today:

| Name | sketch | provider | config | frames | debug |
|------|--------|----------|--------|--------|-------|
| `line-synthetic` | `line` | `synthetic` | `clean` | `[30, 60, 120, 240]` | — |
| `line-synthetic-no-bloom` | `line` | `synthetic` | `clean` | `[30, 60, 120, 240]` | `DISABLE_BLOOM = "1"` |
| `line-screensaver` | `line` | `mock` | `clean` | `[180, 276, 495, 570, 666, 780, 1320, 1770]` | `FORCE_SCREENSAVER` |
| `dots-synthetic` | `dots` | `synthetic` | `clean` | `[30, 60, 120, 240]` | — |
| `dots-screensaver` | `dots` | `mock` | `clean` | `[60, 180, 360, 600, 900, 1320, 1800]` | `FORCE_SCREENSAVER` |
| `cymatics-synthetic` | `cymatics` | `synthetic` | `clean` | `[30, 60, 120, 240]` | — |
| `cymatics-interacting` | `cymatics` | `synthetic` | `clean` | `[60, 120, 240, 480]` | `FORCE_CYMATICS_INTERACTION = "1"` |
| `cymatics-screensaver` | `cymatics` | `mock` | `clean` | `[180, 360, 600, 1200]` | `FORCE_SCREENSAVER = "1"` |
| `radiance-synthetic` | `radiance` | `off` | `clean` | `[60, 120, 240, 480]` | `FORCE_RADIANCE_SYNTHETIC_BODY = "1"` |
| `radiance-screensaver` | `radiance` | `off` | `clean` | `[120, 360, 720, 1200]` | `FORCE_RADIANCE_SYNTHETIC_BODY = "1"`, `FORCE_SCREENSAVER = "1"` |

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

Radiance review guidance (`radiance-synthetic`): (a) a dark glassy humanoid
silhouette with a thin bright rim, mirrored, centered, limbs visibly swinging
across frames; (b) particles emanate outward from the silhouette edge — never
from empty space — rising with a flame-like drift; (c) frames after a
synthetic beat (frame 120 lands just after one) show an outward burst;
(d) `delta_prev` stays well above ~5 (continuous motion). For
`radiance-screensaver`: a slower, thinner ember-toned aura around a gently
drifting phantom; whites/hot tones read ember-orange, and the field is
visibly sparser than the active scenario.

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
| `WC_DEBUG_DISABLE_EXPLODE` | Skip the Dots explode (chromatic-aberration) post-process node, isolating its full-screen fill-rate cost (presence = on; value ignored). |
| `WC_DEBUG_DISABLE_BLOOM` | Zero the main camera bloom intensity. |
| `WC_DEBUG_DISABLE_BONE_COMPOSITE` | Skip the additive bone-glow composite node. |
| `WC_DEBUG_DISABLE_BONE_CAMERA` | Do not spawn the off-screen bone camera. |
| `WC_DEBUG_SOLID_PARTICLES=<rgba hex>` | Render particles as a flat linear colour instead of the star texel. 6 hex digits (alpha defaults to `ff`) or 8 (`rrggbbaa`), no `#`. Bad/odd-length hex -> ignored. |
| `WC_DEBUG_FORCE_SCREENSAVER` | Drive `SketchActivity::Screensaver` at startup so a capture lands in attract mode without waiting out the idle timer (presence = on; value ignored). |
| `WC_DEBUG_FORCE_TIER=<cool\|warm\|hot>` | Pin the screensaver's thermal tier so each tier can be captured deterministically. Unparseable value -> live `ThermalState`. |
| `WC_DEBUG_FORCE_CYMATICS_INTERACTION` | Force the Cymatics primary centre to be held at UV `(0.5, 0.5)` every frame so `active_radius` grows deterministically without hardware or a real mouse press. Used by the `cymatics-interacting` scenario. Presence = on. |
| `WC_DEBUG_FORCE_RADIANCE_SYNTHETIC_BODY` | Drive Radiance from the deterministic synthetic dancer (mask/edges/landmarks/audio) and suppress the mic + camera activation requests. Used by both radiance capture scenarios. Presence = on. |

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
  `{"scenario":"<name>","updated_baselines":true}` on success. If the
  near-zero-luminance guard refuses the update, that's a plain-text error on
  stderr (like any other xtask failure, e.g. an unknown scenario name) and a
  nonzero exit — not a JSON payload.

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
- `--update-baselines` refuses to bless a batch containing a near-zero-luminance
  ("all-black") frame — mean luma under ~1 on the 0..=255 Rec. 601 scale —
  unless `--allow-black` is also passed. This is a guard against the
  backgrounded-window trap below silently poisoning a baseline with an
  unrendered frame; it is not a substitute for actually Reading the frames.
  If the refusal fires, diagnose per the trap below before reaching for
  `--allow-black`.

## Known environment trap: all-black frames when backgrounded

`cargo xtask capture` returns all-black (`[0, 0, 0]`) frames when the app
window is not the foreground/focused window at capture time — this is common
for agent-driven or otherwise headless-ish runs where nothing brings the
launched window forward. `app.log` is clean in this state: no panic, no error,
just a frame that never actually got painted before the screenshot fired.

**This is an environment problem, not a code regression** — don't start
debugging the render pipeline from an all-black frame alone. Diagnostic: run a
known-good sketch/scenario (e.g. `line-synthetic` or `dots-synthetic`, both of
which have committed, previously-confirmed-correct baselines) alongside the
scenario under investigation. If the known-good sketch is *also* black against
its baseline, the capture ran unfocused — refocus the window (or otherwise
ensure the app is foregrounded) and re-run, rather than chasing a phantom
regression in sketch code. If the known-good sketch renders correctly and only
the scenario under investigation is black, then it *is* worth treating as a
real bug.

The `--update-baselines` near-zero-luminance guard above exists specifically
so this trap can't silently commit an all-black PNG as a "baseline" the next
honest capture could then never match — this is exactly how
`dots-synthetic/frame_0030.png` was seeded wrong once (commit `b50a9d63`) and
had to be repaired after the fact (commit `ffd7f3e6`).

## Determinism + headless note

Capture needs a real render surface (macOS dev has a display); the round-trip
smoke check is a dev-machine task, not headless CI. Fixed-`dt` pins the visual
sim (particles, smear, synthetic-hand sweep, `g_constant`); the audio thread does
not affect captured visuals. The xtask enforces a 90s wall-clock timeout on the
launched app as a safety net in case a screenshot observer never fires (then it
errors and points you at `app.log`).
