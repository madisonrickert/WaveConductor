# 8-hour soak analysis — 2026-07-20 (run 20260720-172903)

Full-duration instrumented soak on the Windows dev box (debug profile,
synthetic provider, 30 s sampling, 5 m sketch cycling, display/system sleep
disabled). First long soak with the reworked Radiance flame (~120 k live
particles), the multi-body pipeline, and the day's VRAM-release fixes in the
binary. Artifacts: `target/soak/20260720-172903/`.

## Mechanical verdict vs. read verdict

The harness printed **FAIL**: RSS slope +21.6 MiB/h (r² = 0.60), 129 MiB
gained. Windowed re-fit of `samples.ndjson` shows that number is an artifact
of averaging warmup into steady state:

| window | slope | RSS |
|---|---|---|
| 0–2 h | **+103.6 MiB/h** | 1000 → 1540 MiB |
| 2–4 h | +14.2 MiB/h | 1541 → 1579 |
| 4–6 h | +12.2 MiB/h | 1579 → 1619 |
| 6–8 h | +12.9 MiB/h | 1619 → 1631 |
| 6–7 h | +9.4 MiB/h | — |
| 7–8 h | +7.0 MiB/h | — |

Warmup (every sketch's first entries, shader/pipeline compilation, cache
settling) dominates the whole-run fit — the same effect that makes a 2 m
smoke run report FAIL. Steady state is a **decelerating ~12 → 7 MiB/h
drift**, which does not fit "unbounded leak" but is above the 5 MiB/h
review slope, so it is not a clean bill either.

**Read verdict: REVIEW-grade, no release blocker found.** At the observed
decaying rate the projection is on the order of +100–150 MiB per additional
day — operationally irrelevant for a multi-day kiosk with the watchdog in
place, but worth one more data point (below).

## Every other lane was perfect

- **FPS**: 60.1 → 60.0 active-sketch, 0 % decay over 8 h. (min 30.2 was a
  transient during concurrent dev builds on the same box — the soak shared
  the machine with unrelated compile jobs for part of the run, which taxes
  CPU but not the app's RSS; a pre-event soak should run on an otherwise
  idle box.)
- **Hitches**: zero over 500 ms; worst single frame 407 ms (sketch-switch
  spike, well under the 2 s freeze bar).
- **Freezes/panics**: zero; app clock never stalled; log clean (594 lines,
  no ERROR).
- **Thermal**: cool tier the whole run, 51–53 °C — the multi-hour thermal
  stability target holds with the new flame.
- 95 sketch cycles — the dip-to-black transition and per-sketch
  spawn/despawn survived ~19 full rotations of all five sketches.

## Caveats (per AGENTS.md, what this run cannot claim)

- GPU/VRAM was not sampled. The day's bind-group-cache fixes specifically
  target VRAM-across-transitions; watch dedicated GPU memory by eye during
  the next soak.
- Debug-profile numbers; drift is meaningful, absolute RSS/FPS are not.
- A leak starting late or below ~5 MiB/h remains invisible by design.
- This binary predates the same-day busy-road tracking, tutorial overlay,
  and audio-failover commits.

## Recommendation

Run one more full soak overnight before the Priceless install, on the
final build, on an otherwise-idle box, with a manual VRAM glance at start
and end. If the 2 h+ windowed slope lands ≤ ~10 MiB/h and decelerating
again, treat memory as settled; if it is flat-linear ≥ 15 MiB/h, plot
`samples.ndjson` per-sketch before shipping.
