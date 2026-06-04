# Runbook: Ultraleap "wedge" — symptoms, diagnosis, recovery

**Use this when hand tracking has stopped and you suspect the Ultraleap service is stuck.**
Written from two live wedges observed 2026-06-03 (one spontaneous, one during testing).
For the *why* and the recovery architecture, see
[`../superpowers/specs/2026-06-03-leap-service-recovery-design.md`](../superpowers/specs/2026-06-03-leap-service-recovery-design.md).

---

## TL;DR (30-second triage)

1. **Is it actually a wedge?** Run the probe (no hand needed):
   ```bash
   cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures
   ```
   - `~90 frames/s` → **healthy**, look elsewhere.
   - `0 frames` (before *and* after its `set_pause(false)` attempt) → **wedged**. Continue.
2. **Recover (macOS):** the only reliable fix is to **physically unplug and replug the Leap**.
   A service restart will *not* fix it once the device session is wedged (see below).
3. **Verify recovery:** re-run the probe → expect `~90 frames/s`.

Everything below is the detail behind those three steps.

---

## What a wedge looks like

A wedge is **alive-but-frozen**: the Ultraleap service process is still running and its
*control* path still responds, but the *data* path (the frame stream) is dead. It is **not**
a crash and **not** a USB unplug.

### Healthy baseline (so you know what "normal" is)

| Signal | Healthy |
| --- | --- |
| `libtrack_server` CPU | **~67%** of a core while streaming (even with no hand) |
| Frame stream | **~80–90 frames/s** of (mostly empty) frames, hand or not |
| USB | `Leap Motion Controller` present in `ioreg` |
| Control Panel | both **Application** and **Device Frame Rate** updating |
| Duty cycle on (`WC_LEAP_DUTY_CYCLE=1`, in screensaver) | service CPU ~18%, ~15–30 frames/s (paused ~80% of the time — this is normal, not a wedge) |

### Wedge signatures

There are **two classes**. Tell them apart — they recover differently.

**Class A — daemon-state freeze** (the classic wedge):
- Control Panel says **"tracking active"** but **Device Frame Rate is frozen** while
  **Application Frame Rate keeps updating**.
- `libtrack_server` is **alive** (uptime keeps climbing) but CPU has **dropped from ~67% to ~0%**.
- Frame stream is **0 fps** (probe/monitor show zero).
- `set_pause` calls **keep succeeding** (no API errors) — the control path is alive, the data path is dead.
- **Hand-wave does NOT wake** the app; only non-Leap input (mouse/keyboard) does.
- The device is **still enumerated on USB**.

**Class B — device-session wedge** (often what's left *after* a service restart):
- Control Panel says **"no leap detected."**
- `libtrack_server` is running (possibly freshly restarted, low uptime) but **~0% CPU** and not binding the device.
- The device is **still enumerated on USB** — the daemon sees the descriptor but can't acquire it.

> The progression we observed: a Class A freeze, then a `kickstart -k` restart cleared the
> *daemon's* state but left a Class B "no leap detected" — because the **device's** USB session
> was wedged, not the daemon. Only a replug cleared it.

---

## Diagnose (all zero-privilege, copy-paste)

```bash
# 1. Is OUR app even involved? (Both wedges we saw needed no WaveConductor at all.)
pgrep -fl waveconductor || echo "  -> WaveConductor not running"

# 2. Service alive? CPU? uptime?  (2 samples; the 2nd is the accurate instantaneous CPU.)
#    ~0% CPU + high uptime = frozen-but-alive (Class A). Fresh + ~0% = Class B.
top -l 2 -s 1 -stats pid,cpu,time,command -o cpu | grep -i libtrack

# 3. Still on the USB bus?  Look for "Leap Motion Controller".
#    Present  -> data-path/device-session wedge (NOT a disconnect).
#    Absent   -> a real USB drop; replug is the only option anyway.
ioreg -p IOUSB -l -w0 | grep '"USB Product Name"'

# 4. Definitive liveness + rung-1 recovery test (no hand, no focus needed):
cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures
#    0 frames before AND after  -> wedged, rung 1 can't fix it.
#    ~90 frames/s               -> healthy.

# 5. (Optional) Watch continuously while reproducing / under load:
cargo run -p wc-core --example leap_heartbeat_monitor --features hand-tracking-gestures
#    Prints frames/s each second; loudly flags a WEDGE after 2s of zero frames.
```

The two probe examples request the `BackgroundFrames` policy, so an unfocused CLI is **not**
falsely read as silent — `0 frames` from them genuinely means the service isn't streaming.

---

## Recover (macOS) — cheapest first

| Rung | Action | Works on a wedge? |
| --- | --- | --- |
| 1 | **Client reconnect** — `leap_recovery_probe` connects a fresh client + `set_pause(false)` | ❌ tested live: 0 frames before and after |
| 2 | **Per-device USB reset** | ❌ not available on macOS |
| 3 | **Restart the daemon** (needs your password): `sudo launchctl kickstart -k system/com.ultraleap.tracking.service` | ❌ for a Class B device-session wedge — daemon comes back "no leap detected" |
| — | **Physically unplug + replug the Leap** | ✅ **the only reliable fix on macOS** |

Recovery steps:

```bash
# (Only worth trying on a Class A freeze; skip straight to replug for Class B / "no leap detected".)
sudo launchctl kickstart -k system/com.ultraleap.tracking.service
cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures   # verify

# The reliable fix:
#   Physically unplug the Leap, wait ~2s, replug. Then verify:
cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures   # expect ~90 fps
```

The GUI Control Panel's "restart service" button (if present) is only the user-facing
equivalent of `kickstart` — same Class-B limitation. There is **no fully-automated recovery
for a device-session wedge on macOS** (no software USB re-enumeration); this is a known
deployment gap. Linux (`sysfs authorized` / `usbreset` / udev) and Windows
(`pnputil /restart-device`) *can* re-enumerate in software.

Service identity for reference: `com.ultraleap.tracking.service` — a **root LaunchDaemon**
(`/Library/LaunchDaemons/`), `KeepAlive = true` (so a plain kill respawns it).

---

## What triggers it (and what doesn't)

- **Trigger: GPU / concurrency contention.** Ultraleap's own *Known Issues (Gemini)* documents
  the service "intermittently stops when using some applications (e.g. OBS) at the same time."
  We saw it wedge **spontaneously during hot multitasking with WaveConductor not running at all.**
- **Not (reliably) the duty cycle.** With `WC_LEAP_DUTY_CYCLE=1`, the duty cycle churned the
  service ~2×/s for **30 minutes with no wedge** — and hit its thermal target (~67% → ~18% CPU).
  So treat the wedge as an **intermittent GPU-contention-spike event**, not something the duty
  cycle causes on its own. (The duty cycle is **off by default**; opt in only to measure it.)
- **Prevention:** avoid heavy concurrent GPU apps during unattended kiosk operation; keep the
  duty cycle off unless you've accepted the wedge-recovery story for the target OS.

---

## If you hit a new wedge, capture this

To grow our evidence base, before recovering jot down:
- Which **class** (A "tracking active but frozen" vs B "no leap detected").
- `libtrack_server` **CPU and uptime** (command #2 above).
- Whether the device was **still on USB** (command #3).
- What was **running / GPU load** at the time.
- Whether **rung 3 (restart)** did anything, and that **replug** fixed it.

Add a dated bullet to this file or the design doc's "Live wedge recovery test" section.
