# Runbook: Dots (Fabric) FPS oscillation — GPU saturation, not thermal

**Use this when** the Dots/Fabric sketch feels sluggish or its frame rate
oscillates (commonly 72↔15-20 fps, present even at idle with no hand up), or
the next time a sketch's frame time is bimodal/spiky and you need to know
whether it is GPU-bound, CPU-bound, thermal, or present-paced.

Diagnosed June 2026 on an Apple-Silicon Retina laptop. The instrument added
for it is the `WC_DEBUG_DISABLE_EXPLODE` toggle (commit `47c42682`); the
caching follow-ups (`129c91ba`, `e22ef12e`) and the `gamma`-skip (`38976f0a`)
came out of the same investigation but did **not** fix the oscillation.

---

## TL;DR

- **Root cause: GPU *compute saturation* at maximum clock, not thermal
  throttling.** The explode (chromatic-aberration) post-process pushes the
  Dots frame's GPU work to ~14 ms at max clock — about 85% of the 16.6 ms
  60 fps budget. With the GPU already pegged at its top clock, there is no
  headroom to absorb frame-to-frame variance, so frames intermittently slip
  the vsync deadline → the 27-42 ms spikes.
- **It is NOT:** the explode shader's ALU cost, bloom, Retina fill-rate, the
  per-frame `create_bind_group` churn, or OS thermal throttling. All ruled
  out (see below) so you don't re-investigate them.
- **Fix = restore GPU headroom.** Preferred: **cap the framerate (30-40 fps)**
  — gives the GPU a bigger per-frame budget, lets it drop to a lower clock,
  kills the spikes, and lowers soak power/heat. Secondary: reduce per-frame
  GPU work (lower internal resolution / fewer particles / cheaper passes).
- **Do NOT chase it with shader micro-opts or fewer explode iterations** —
  the cost is texture *bandwidth* and overdraw, not ALU, and cutting
  iterations visibly breaks the signature radial-zoom look (tested: 5→3
  collapses the streaks to discrete sparkles).

---

## The evidence (Metal System Trace A/B)

Recorded with `xctrace` (full Xcode required), Dots idle, attached to a
running `cargo rund`, explode ON vs OFF (`WC_DEBUG_DISABLE_EXPLODE=1`):

| GPU performance state | Explode ON | Explode OFF |
|---|---|---|
| **Maximum clock** | **98.3%** of time | **80.9%** |
| Medium clock | 0.3% | **17.0%** |
| Minimum clock | 1.4% | 2.1% |
| Device thermal state | constant (1 interval) | constant |

Reading: explode ON pegs the GPU at max clock (no headroom); explode OFF lets
it sit at medium clock 17% of the time (spare capacity that absorbs variance).
The device thermal-state never changed in either run, so this is **not** OS
thermal pressure — it is the GPU running flat-out because there is enough work
to keep it there. The `Maximum`-state dwell median was 14.2 ms ≈ one frame.

This reconciles the earlier confusing findings: render GPU-busy was measured at
~14 ms/frame (yes — that's 85% of the budget, near-saturated), no single GPU
stage interval exceeded ~9 ms (the cost is the *sum* of many fetches, not one
pass), and the spikes were "present/scheduling" (yes — a saturated GPU with no
clock headroom slips the deadline under jitter).

## What was ruled out (don't redo these)

- **Shader ALU cost** — median frame-time delta on/off is ~0.3 ms; the
  `pow`/`normalize` micro-opts save ~nothing. The cost is bandwidth/overdraw.
- **Bloom** — a 2×2 (explode × bloom via `WC_DEBUG_DISABLE_BLOOM`) showed
  explode is the trigger; bloom is irrelevant to the spikes.
- **Retina fill-rate** — forcing a true 1920×1080 framebuffer
  (`WindowResolution::new(1920,1080).with_scale_factor_override(1.0)`) did not
  remove the spikes and the median frame time did not drop. (The median
  staying at 16.6 ms hints at a smaller secondary resolution-independent
  pacing factor — CPU/present/MediaPipe — on top of the Retina GPU saturation.)
- **Per-frame `create_bind_group`** — a bind-group cache (now landed as
  soak-stability hygiene in `129c91ba`/`e22ef12e`) did **not** reduce the
  spikes. It was never the cost.
- **OS thermal throttling** — device thermal-state constant across the run.

## How to reproduce / re-measure

Frame-time distribution (no extra tooling): temporarily set the window to
`PresentMode::AutoNoVsync` and add a system that logs the smoothed
`FrameTimeDiagnosticsPlugin::FRAME_TIME` to stdout each frame, then run
`WAVECONDUCTOR_START_SKETCH=dots cargo rund` and compare explode on/off (filter
out the first ~5 s of warm-up).

GPU performance-state A/B (the decisive one):

```bash
# launch idle in Fabric, then attach a Metal System Trace for ~20 s
WAVECONDUCTOR_START_SKETCH=dots cargo rund &
PID=$(pgrep -f target/debug/waveconductor); sleep 8
xcrun xctrace record --template "Metal System Trace" --attach $PID \
  --time-limit 20s --output /tmp/dots.trace
# export the performance-state + thermal tables
xcrun xctrace export --input /tmp/dots.trace \
  --xpath '/trace-toc/run[@number="1"]/data/table[@schema="gpu-performance-state-intervals"]'
```

Gotchas:
- The trace can fail to finalize if the app is killed too eagerly — let
  `xctrace` hit its own `--time-limit` and add a ~2 s delay before `pkill`.
- The xctrace export uses an id/ref dedup scheme; resolve `ref="N"` against
  the first element that defined `id="N"`.
- To exercise the bone path (or any hand-driven visual) without hardware, use
  `WAVECONDUCTOR_HAND_PROVIDER=synthetic` (stationary open-hand fixture).

## The fix — configurable frame-rate cap (implemented)

The lever is **headroom**, and the way we buy it is a **configurable frame-rate
cap**: `crates/wc-core/src/frame_limiter/` (commit `3572dfec`). A `Last`-schedule
system sleeps the main loop to hold at most `FrameLimiterSettings::target_fps`
(global, persisted, `category = User`, **default `60` fps**, in the "Display"
panel section; `0` = uncapped). Change it in the panel, or pin it at launch with
`WAVECONDUCTOR_FPS_CAP=30`. Sleep-only + drift-free pacing; native only (web is
rAF-paced). We rolled our own rather than `bevy_framepace` (its newest release
targets Bevy 0.18; we're on 0.19) — see the module docs.

**Verified** on Dots idle: `WAVECONDUCTOR_FPS_CAP=30` holds a steady 30.00 fps
(33.33 ms, flat) and the GPU performance-state goes from **98.3% Maximum clock
(uncapped) → 2.2% Maximum / 63.7% Minimum** — the GPU idles most of each frame,
restoring the headroom that absorbs variance and dropping power/heat sharply.

Reducing iterations is **not** a viable alternative: tested 5→3, it collapses
the signature radial-zoom streaks into discrete sparkles (the streaks *are* the
multi-iteration accumulation), and it's a partial lever anyway (the explode is
one of several GPU costs). Reducing other per-frame GPU work (internal-resolution
scale, particle count) is the option if a locked 60 fps is ever required, but it
trades visual density for headroom.
