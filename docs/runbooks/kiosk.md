# Kiosk deployment runbook

Running WaveConductor unattended on a Windows kiosk/installation box (written
for the Priceless deployment, 2026-07 week). Two halves: the **watchdog** that
keeps the app alive, and the **box checklist** that keeps Windows from
interfering.

## Watchdog

`scripts/kiosk-watchdog.ps1` ships in the Windows bundle beside
`waveconductor.exe`. It:

- launches the app and **restarts it on any exit** (panic, GPU device loss,
  driver reset) with exponential backoff (5 s doubling to 60 s; a run that
  stays up 10 minutes resets the backoff);
- **detects hangs** without any app-side support: if the window reports
  not-`Responding` continuously for 60 s, the process is killed and
  relaunched;
- logs to `kiosk-watchdog.log` beside the exe with size-capped rotation
  (5 MB, one previous generation), so multi-day runs cannot fill the disk.

Setup on the kiosk box (from the install directory):

```powershell
powershell -ExecutionPolicy Bypass -File kiosk-watchdog.ps1 -Install   # logon task
powershell -ExecutionPolicy Bypass -File kiosk-watchdog.ps1            # or run now
```

`-Uninstall` removes the scheduled task. The task runs at user logon; pair it
with Windows auto-logon (netplwiz) so a power cycle boots straight back into
the app.

## Box checklist (before the event)

Power / display:
- `powercfg /change monitor-timeout-ac 0` and `standby-timeout-ac 0`
  (display and system sleep off).
- Screen saver OFF (Settings > Personalization > Lock screen).
- If the display is portrait, set the rotation in Windows display settings;
  the app adapts (picker reflows, sketches are aspect-correct as of
  2026-07-20).

Interruptions:
- Focus Assist / Do Not Disturb ON (alarms only) so toasts never cover the
  app.
- Windows Update: set Active Hours to cover the event window; ideally pause
  updates for the week.
- Remove/disable anything else that auto-starts with a window.

Audio / camera:
- Pin the intended output device as Windows default before launch.
- OBSBOT: the app takes control of the camera itself when built with
  `obsbot-camera-control` (see docs/runbooks/obsbot.md); otherwise disable
  its on-device AI in OBSBOT Center once (persists on-device).
- Ultraleap: Gemini tracking service set to start automatically.

App configuration:
- Settings live in the user config dir (`sketch-settings.toml`); configure
  on-site via the settings panel (gear icon), including camera framing.
- Display settings: BorderlessFullscreen on the target monitor.

Smoke test: launch via the watchdog, kill `waveconductor.exe` in Task
Manager, confirm the watchdog relaunches it within ~10 s and logs both events.

## Crowded venue tuning (busy road / heavy background traffic)

Body tracking prioritizes people **closer to the camera** and **moving
more**; background walk-through traffic is gated so it cannot cause visual
churn. What to reach for on-site, in order:

- **Background subdue** (Radiance settings, Simulation section, live): how
  strongly a standing-still body's flame share is subdued in favour of
  moving dancers. Default 0.5 (a still body burns at ~80% relative weight);
  1.0 subdues hardest (~60%); **0 turns the behaviour off entirely** if it
  misfires. It only redistributes between bodies — a lone person is never
  dimmed by it.
- If passers-by still flash into the sketch, the **admission dwell** is the
  gate: a new detection must persist 700 ms before its fade-in starts, and a
  sub-600 ms track frees its slot instantly (no reserved-slot zombie).
  Constants (rebuild required — deliberately not live knobs, because the two
  must keep their documented ordering): `ADMIT_DWELL` in
  `crates/wc-core/src/input/body/envelope.rs` and `RESERVE_MIN_ACTIVE` in
  `crates/wc-core/src/input/body/pipeline.rs` (keep it ≥ ~100 ms *below*
  `ADMIT_DWELL`).
- If the wrong person holds the featured (primary) spot, the motion bias
  constants live in `crates/wc-core/src/input/body/selection.rs`:
  `MOTION_FLOOR` (0.55 — raise toward 1 to weaken the mover bias, lower to
  strengthen it) and the `MOTION_SPEED_LO`/`MOTION_SPEED_HI` ramp (0.2/1.0,
  sqrt-size-normalized screen units/s — **calibrated on synthetic fixtures
  only; verify against live bodies on the deployment camera**). The `KeyN`
  hotkey still manually pins any tracked person as primary.
- Aiming the camera to keep the road out of frame remains the best fix:
  any detected person (admitted or not) still resets the idle timer, so a
  road in view means the attract screensaver never engages.

## Known reliability posture (2026-07-20)

- 8 h instrumented soak on this hardware: see `target/soak/` latest run and
  the analysis in `docs/superpowers/` (soak analysis doc of the same date).
- Leap duty-cycle pause is OFF by default (wedges the device; recovery ladder
  unbuilt — the watchdog does NOT cover a wedged Leap, only a wedged app).
  If hands die but visuals run, power-cycle the Leap's USB.
- Audio device failover: the app rebuilds its output stream if the device
  disappears (2026-07-20); if audio is still silent, check the Windows
  default device and restart via watchdog kill.
