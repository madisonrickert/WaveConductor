# OBSBOT camera control

Programmatic control of an OBSBOT camera (Tiny 2 Lite at the last deployment)
so its on-device AI stops fighting the app's own MediaPipe tracking. Behind
the `obsbot-camera-control` cargo feature on `wc-core`; real device IO is
Windows-only (the deployment target) — every other platform compiles a
documented no-op facade so CI's `--all-features` stays green.

## What it does

On startup (and on hotplug), a dedicated `obsbot-control` worker thread finds
the first OBSBOT device via the vendored libdev SDK and runs the take-control
sequence, in this order (the SDK requires AI off before manual gimbal control
holds):

1. **AI tracking OFF** — `cameraSetAiModeU(AiWorkModeNone)` (tiny2
   series/tail air) or `aiSetTargetSelectR(false)` (gen-1 tiny), plus
   `aiSetEnabledR(false)`.
2. **Gesture control OFF** — master switch and each individual gesture.
3. **Gimbal recenter** — `gimbalRstPosR()`.
4. **Widest FOV (86°)** — `cameraSetFovU(FovType86)` + digital zoom reset to 1.0.
5. **Auto exposure ON** — explicitly re-asserted (`DevExposureAllAuto` where
   the firmware accepts it, else AE-unlock + face-AE for the tiny series).
   Auto exposure is never disabled.

Each step logs an INFO line (`OBSBOT take control: <step>: ok|FAILED`) — that
is the operator's confirmation at a gig. The `ObsbotControl` Bevy resource
reports `NoDevice` / `TakingControl` / `InControl{sn, firmware, product}` /
`Failed{achieved}` / `ControlDisabled{sn}`. "In control" requires the AI-off
and gesture-off steps; FOV/gimbal/exposure failures only warn.

On clean shutdown (Bevy `App` drop → `ObsbotControl` drop → worker join) the
camera is **restored to its out-of-the-box behavior** — AI tracking and
gestures re-enabled — so OBSBOT Center or the next app isn't surprised. A hard
process kill skips this; see recovery below.

A settings toggle — **Camera → "Take control of OBSBOT camera (disable its
on-device AI)"**, default ON, persisted under the `obsbot` storage key —
releases/re-takes control live.

## Manual framing controls (settings panel)

The Display tab's **Camera** section (settings panel → DISPLAY) carries four
framing controls, persisted with the `obsbot` settings:

- **Gimbal pitch** — −45°..45° (the device accepts −90..90; the slider is
  capped to the useful desk/kiosk range so slider travel isn't wasted on
  ceiling/base angles).
- **Gimbal yaw** — −90°..90° (device range −180..180, same rationale — a
  desk-mounted camera never usefully points backwards).
- **Zoom** — 1.0..2.0 absolute digital zoom.
- **Field of view** — Wide86 / Medium78 / Narrow65 preset dropdown.

Behavior:

- **Live-apply, coalesced.** Slider changes are diffed against the last values
  actually sent and forwarded to the worker at most every 100 ms,
  latest-wins — a drag never queues stale intermediate positions behind the
  SDK's blocking setters (`crates/wc-core/src/input/obsbot/framing.rs`).
- **Gated on control.** The sliders are always editable (the
  reflection-driven panel cannot disable individual rows), but commands are
  only sent while the status is `InControl`; otherwise they are held and the
  status line below the section says why (no camera detected / control
  disabled / take-control failed).
- **Framing is re-applied on every (re)acquisition of control.** The
  take-control sequence recenters the gimbal and resets FOV/zoom (steps 3–4
  above), so the moment the status enters `InControl` — app start, hotplug
  re-detect, or the take-control toggle flipping back on — the stored framing
  is re-sent in full. An installation that restarts comes back with the
  operator's framing, not the factory center. (Resetting the stored settings
  to the recentered defaults was considered and rejected: it would discard
  the operator's framing on exactly the restart that matters.)

A status line under the section shows the live device state (product, serial,
firmware while in control; "No OBSBOT camera detected" otherwise).

### Camera preview

**Camera → "Camera preview"** (default OFF, storage key `camera_preview`)
shows a ~320 px live view of the tracking camera inside the settings panel.
It works with **any** webcam, not just an OBSBOT: it taps the frames the
hand/body tracking worker is already capturing (so it only shows an image
while tracking is running — enter a sketch first). While the toggle is off
the workers skip all preview work (one atomic check per frame); while on,
frames are downscaled and published at ≤10 Hz
(`crates/wc-core/src/input/camera_preview.rs`).

The manual APIs behind the sliders (`ObsbotControl::set_gimbal_angle/-speed`,
`gimbal_stop`, `set_zoom`, `set_fov`) remain available for future
choreography code.

## Code map

- `vendor/libdev/` — vendored OBSBOT SDK (C++11 API, v1.3.0): headers,
  per-platform binaries, and the upstream `OBSBOT_Sample`.
- `vendor/libdev/shim/obsbot_shim.{h,cpp}` — hand-written extern "C" facade
  (bindgen cannot consume the C++ API). No exception crosses the boundary;
  step results come back as a bitmask.
- `crates/wc-core/build.rs` — compiles the shim (cc, `/MD`, exceptions
  contained) and links `windows/win64-release/libdev.lib`, Windows +
  feature only; stages the runtime DLLs (below).
- `crates/wc-core/src/input/obsbot/` — Bevy plugin, status resource,
  settings, worker thread (`platform/windows.rs`), no-op facade
  (`platform/stub.rs`).

## Deploy notes

- **DLLs beside the exe.** `libdev.dll` **and** `w32-pthreads.dll` (from
  `vendor/libdev/windows/win64-release/`) must sit next to
  `waveconductor.exe`. Dev/test builds are covered: wc-core's build.rs copies
  both into `target/<profile>/` and `target/<profile>/deps/`. Packaging is
  covered: `cargo xtask bundle-windows` stages both DLLs from the binary dir
  into the dist folder when the feature was compiled (a feature-off build
  ships neither), and the MSI harvests the staged dir. Mind the license
  caveat below before shipping a feature-on MSI publicly.
- The feature is **not** in `default`; enable it on the app build that runs
  with the OBSBOT connected.
- Device enumeration is asynchronous (~3 s after SDK init); the worker also
  rescans on hotplug events and on a 5 s backoff, so plugging the camera in
  after launch is fine.

## License caveat — resolve before public redistribution

`vendor/libdev` ships **no license file**, and `dev.hpp` contains an internal
marker (near the `MtpFileType` enum, ~line 136) that translates to "delete
this section when providing the SDK externally; not open to the public" —
i.e. the vendored drop looks like an internal/partner build of the SDK.
**Redistribution terms must be clarified with OBSBOT before any public
release ships these headers or binaries.** Local/gig use on our own hardware
is the current scope.

## Hardware smoke test

With a camera plugged in:

```
cargo test -p wc-core --features obsbot-camera-control obsbot_hardware_smoke -- --ignored --nocapture
```

Ignored by default (needs hardware). It inits the SDK, takes control (the
gimbal should physically recenter), holds 2 s, releases (AI/gestures back
on), and prints every return code.

## If SDK control fails (fallback)

If the status is `Failed` (or the camera visibly keeps tracking/reacting to
gestures), disable the on-device features manually in **OBSBOT Center** on
the deployment box: set AI tracking mode to *None*/off and turn off gesture
control, then recenter the gimbal and select the 86° FOV there. Those
settings persist on the device across power cycles, which also serves as
recovery after a hard app kill that skipped the restore step (or the
opposite: use OBSBOT Center to re-enable AI/gestures if a kill left them
off and another app expects them).
