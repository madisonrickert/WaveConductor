# Deferred / Unfinished Work Inventory — 2026-07-20

Compiled by an automated sweep of code comments (`TODO`/`FIXME`/deferred markers),
`docs/superpowers/next-plan-carry-forwards.md` (Madison's 90-item running list),
`docs/superpowers/roadmap.md`, runbooks, `deny.toml`, and disabled tests — for
Madison to review and approve/deprioritize. Items already resolved by the
2026-07-19/20 unattended sessions are marked ✅ RESOLVED at the bottom.

## Highest priority (kiosk reliability + release gates)

1. **Leap deep-idle / wedge recovery** — the screensaver duty-cycle pause is
   gated OFF behind `WC_LEAP_DUTY_CYCLE=1` because live testing (2026-06-03)
   wedged the device (USB replug needed). Detection landed
   (`PrimaryState::DeviceWedged`); the recovery ladder (`leap-watchdog-recovery`)
   is unbuilt. Roadmap `leap-deep-idle-state`, top priority.
   ⚠️ Carry-forward **#84 is STALE and harmful**: it advises re-wiring the pause
   systems as "dead code" — they are wired (behind the env gate) and deliberately
   off because they wedge the device. Do not action #84.
2. **Touch & hand-gesture can't activate the Line attractor** (carry-forward
   #60; `line/systems/mouse.rs` reads only `MouseButton::Left`). Blocks the
   touchscreen-kiosk primary interaction. Needs `Touches` + a pinch/fist press.
3. **Audio device failover unbuilt** (roadmap 1.A) — mid-run cpal errors surface
   as `AudioStatus::Errored` but the stream is never rebuilt; an install runs
   silent for hours if the output device disappears.
4. **Full-render soak telemetry** (roadmap Phase 2; carry-forwards #87/#88) —
   the existing 8 h soak harness drives the real app now, but the roadmap's
   CSV frame-time p95/p99 + thermal-band telemetry lane and the DRS/perf-governor
   phase it gates remain open.
5. **Disabled correctness test** `wc-core/tests/input.rs:280`
   (`#[ignore]`, Plan 6 TODO) and the `#[ignore]`d pre-tag
   `line_soak_with_overlay_ui` (carry-forward #71) are release-gate items.

## Bug-risk (smaller)

6. `mediapipe/pipeline.rs:119` — `MIN_TRACK_LANDMARK_SPREAD = 0.04` needs a
   hardware-validated re-pick (candidate 0.03); may drop an edge-on hand.
7. `LineRestartPending` trampoline cleanup race (carry-forward #10) — narrow,
    needs timestamp-and-reap or a one-shot.

## Performance

8. Thermal Hot-tier is "Low-Rate Ember", not a true dispatch freeze —
   explicitly YAGNI until soak data says otherwise (`lifecycle/thermal/mod.rs`).
9. Fast linker (mold/lld) not wired (carry-forward #86) — merge into the
   existing per-target rustflags (don't replace; LeapC rpath).
10. `MAX_ATTRACTORS` uniform→storage-buffer switch if count grows past ~16
    (`particles/particle.rs:80`).

## Packaging / distribution

11. Release-gate matrix incomplete: macOS DMG notarization, portable exe,
    AppImage, web bundle, CI signing.
12. `main.rs:144` "Windows builder deferred as a build-prerequisite decision"
    comment — decision point for Madison (wix/ + xtask msi now exist and stage
    all runtime DLLs; likely just needs the comment retired and the decision
    ratified).
13. `leap-sdk-archive` — offsite archive of the Ultraleap 6.2.0 installers
    (abandonware hedge). Ops task, no code.

## Polish / feature ports (selected)

14. Heatmap-image native file picker (carry-forward #62) — `rfd` dialog instead
    of free-text path. Madison wants to iterate on heatmaps visually.
15. Fullscreen-toggle overlay button (carry-forward #64) — keybinding exists,
    button never shipped (~40 LOC).
16. Info/About overlay (carry-forward #65) — decide: dedicated panel vs the
    credits tile (a full credits screen shipped 2026-07-20; likely drop #65).
17. Auto-reenter sketch on `requires_restart` change (carry-forward #3) —
    same-frame OnExit→OnEnter instead of punting to Home.
18. Attractor rings rotationally symmetric so spin is invisible (carry-forward
    #56; v4 rings visibly spin) — needs low-segment mesh + v4 check.
19. Per-sketch screensaver attract performers — only Line has one; Flame /
    Dots / Cymatics / Radiance want their own (roadmap `screensaver-attract`).
20. HandMesh port to Dots/Cymatics (+bloom path) (carry-forwards #74/#75).
21. Provider fusion is a passthrough (carry-forward #76) — per-chirality
    precedence unbuilt until a second provider registers.
22. Reflection panel: number types beyond `u32/f32/f64` render "(unsupported)"
    (carry-forward #2); dev panel lacks section grouping (#66).
23. `line_synth.rs` deferred pieces: chord stack, compressor knee/ratio match,
    background-mp3 mixer.
24. `WebSocketProvider` stub (web-target input layer).
25. Micro-polish tail: carry-forwards #4–5, #7, #11–52 (each tiny; absorbed by
    plan Phase-0s).

## Body-tracking perf candidates (from the 2026-07-20 parity audit — parity itself is CONFIRMED clean)

- fp16 conversion of both pose models for DirectML/CoreML — likely material
  iGPU speedup; needs a capture-harness A/B for accuracy drift first.
- ort IOBinding with pre-allocated outputs to remove the ~0.9 MB/frame
  dependency-forced output copy in `onnx/ort.rs` (now documented inline).
- `square_pad_into`/`warp_roi_into`: row-slice copies instead of per-pixel
  `put_pixel`, and skip the pad when the frame is already square.
- ATTRIBUTION.md notes the landmark model variant (lite vs full) is
  unverified — if tracking feels weak on hardware, re-vendor the explicit
  `full` from PINTO 053_BlazePose per the documented procedure.

## Docs / sign-off

26. PARITY.md verdicts PENDING across Cymatics/Flame/Dots/Line/HandMesh —
    operator hardware/visual sign-off + the `line-parity-signoff` capture that
    tags `v5-line-parity`. AgX palette tuning is operator-deferred.
27. Madison-owned manual checks: gravity smear on press+drag (#55), heatmap
    image end-to-end (#63).
28. Stale plan-doc patches (#29, #36); `groupedUpness` spelling check (#15).
29. `docs/adr/` promises "first ADR in Plan 2" — never started; backfill or
    drop the promise.

## Supply-chain deferrals (deny.toml ignores, re-evaluate on dep bumps)

30. `RUSTSEC-2024-0436` (`paste` unmaintained, via wgpu/Bevy).
31. `RUSTSEC-2026-0192` (`ttf-parser` unmaintained, via bevy_text).
32. `RUSTSEC-2026-0194/0195` (`quick-xml` DoS, Linux build-time only via
    wayland-scanner; not exploitable here).

## ✅ Resolved by the 2026-07-19/20 sessions (found stale during the sweep)

- Roadmap `soak-test-command` — `cargo xtask soak-test` exists and is
  documented in AGENTS.md.
- Picker "Open Source Licenses" TODO (`picker.rs`) / roadmap "Licenses
  surface" — a full credits/licenses overlay screen shipped 2026-07-20.
- OBSBOT DLL WiX packaging TODO (`docs/runbooks/obsbot.md`) — bundle-windows
  stages `libdev.dll` + `w32-pthreads.dll`; MSI harvests them.
- Carry-forwards #45, #58, #59, #70 — already marked RESOLVED in-doc.
- `AppState::Waves` seam is intentional (guard-tested), not stale.
- Verified already-implemented on 2026-07-20 (carry-forwards doc was stale;
  each has tests in-tree): #1 save-on-exit flush (`autosave.rs
  flush_on_exit`), #57 per-field serde defaults (all 12 settings structs +
  regression tests), #53/#54 Line post-process gating + persistent uniform
  buffer, #8 release asset path (`platform/assets.rs asset_root()` 5-step
  resolver incl. `WAVECONDUCTOR_ASSET_ROOT` override).
