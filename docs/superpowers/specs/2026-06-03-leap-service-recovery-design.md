# Leap deep-idle state & wedge detection/recovery — design

**Status:** Research / design (roadmap item `leap-deep-idle-state`). Supersedes the naive `leap-idle-pause` duty-cycle approach, which live-testing falsified (see below).
**Date:** 2026-06-03.
**Scope:** all three major desktop OSes are **first-class** (macOS, Linux, Windows). iPad is out of scope for *this* item — its hand tracking is Apple Vision (in-process), so there is no external Leap service to wedge or restart.

---

## Why this exists

Performance tuning during **deep idle** is a core v5 goal, and the **state of the Ultraleap tracking service is a critical part of it** — the service is a heavy constant host-CPU load (measured ~52–65% of a core on the dev M1, even with no hand present). Shedding that during the screensaver is a real thermal lever. But the service is also a *fragile external dependency we don't control*, and managing its state during deep idle turned out to be the hard part.

### What live-testing established (2026-06-03)

We built and live-tested a duty cycle that pauses the Leap service during the screensaver and briefly un-pauses it (~0.5 s period) to sample for a returning hand. Findings:

1. **Phase 2 fix (shipped, commit `e9831ab5`):** `reset_on_interaction` previously counted *any* `HandTrackingFrame` as interaction, but a running Leap streams empty frames continuously — so a connected Leap pinned the idle timer and the screensaver **never triggered on a real install**. Fixed: only hand-bearing frames reset the timer. This is correct and independent of everything below.
2. **The live duty cycle wedged the device.** Under sustained ~2/s `set_pause` toggling, `libtrack_server` dropped to ~0% CPU, the device frame stream froze, the hand-wave wake failed (only the mouse — a non-Leap path — woke it), and recovery required a **physical USB replug**. The `set_pause` *control* path kept ack'ing (1233 calls, zero API errors) while the *data* path died.
3. **But rapid `set_pause` toggling is NOT inherently the cause.** A standalone stress harness (`crates/wc-core/examples/leap_duty_stress.rs`) swept window/period combinations down to the exact aggressive live setting (150 ms window / 350 ms gap) and the device tracked ~97% of windows and **stayed alive across every config**. So the wedge is **live-app-specific**, not a property of the toggle rate.
4. **Strong root-cause lead — GPU/concurrency contention.** Ultraleap's own *Known Issues (Gemini)* documents that the tracking service "intermittently stops when using some applications (e.g. OBS) at the same time," plus a memory leak on every client connect/disconnect or tracking-mode change ("resolved by restarting the service"), plus that the Control Panel can report the service stopped *even though it restarted* (i.e. the status indicator lies — matching what we saw). The differentiator between our healthy CPU-only harness and the wedging live app is the **full GPU render pipeline**. This is now a concrete, testable hypothesis. *(Sources at the bottom; their site blocks automated fetch — read in a browser.)*

### Harness gotcha worth remembering

LeapC only streams frames to the **focused** application unless the `BackgroundFrames` policy is requested. A CLI harness never has focus (and the Control Panel may be focused), so without `request_background = true` it reads **zero frames** — which looks identical to a device wedge. The real app sets this; diagnostic harnesses must too.

### Live wedge recovery test (2026-06-03, second session)

A wedge occurred **spontaneously** during ordinary hot multitasking, with **WaveConductor not running at all** (confirmed: no `waveconductor` process). This is important:

- It **exonerates our duty cycle** for *this* wedge and corroborates the GPU/concurrency-contention root cause — the vendor service froze on its own under load, matching Ultraleap's documented "service intermittently stops when using some applications at the same time."
- **Consequence for the roadmap:** wedge-recovery is needed *regardless* of what we decide about pausing. Even if we drop the duty cycle entirely, the kiosk's hand-tracking dependency can freeze under load over a long unattended run, so detection + recovery is a standalone requirement, not a cleanup for our own toggling.

Characterization of the live wedge (all zero-privilege diagnostics):

- `libtrack_server` **alive** (81 min uptime) but pinned at **~0% CPU** (was ~67% moments earlier) — the frozen-but-alive signature, not a crash or clean exit.
- The Leap is **still enumerated on USB** ("Leap Motion Controller" present) — so this is *not* a USB-level disconnect; the wedge is purely in the service's data path. The human replug works by forcing the service to rebuild its device session, not because the device fell off the bus.
- Service identity confirmed: `com.ultraleap.tracking.service`, a root LaunchDaemon in `/Library/LaunchDaemons/`, `KeepAlive = true` + `RunAtLoad = true` (so killing it auto-respawns).

**Rung-1 recovery, empirically tested** (`crates/wc-core/examples/leap_recovery_probe.rs` — a hand-free liveness probe that connects a fresh client, observes the data path, then tries `set_pause(false)`):

- Fresh client `start()` **succeeded** (control path alive).
- Data path: **0 frames in 3 s** as-found, **0 frames in 4 s** after `set_pause(false)`.
- **Verdict: rung 1 does NOT clear a service-level wedge.** A client reconnect / pause-clear is insufficient for this failure mode. (Keep rung 1 in the ladder for client-side staleness — a different failure — but it will not fix the daemon freeze.)

`sudo` was not cached, so rung 3 was *not* run autonomously; the exact command is staged for the operator (below), with `leap_recovery_probe` as the post-restart verification (a revived service streams frames immediately, no hand needed).

#### Operator runbook — recover this wedge (macOS, needs your password once)

```bash
# Rung 3 — restart only the Ultraleap daemon (third-party, so the macOS 14.4
# kickstart -k restriction on Apple-critical daemons does not apply):
sudo launchctl kickstart -k system/com.ultraleap.tracking.service
# If that errors, KeepAlive=true means a plain kill respawns it:
#   sudo launchctl kill SIGKILL system/com.ultraleap.tracking.service
# Full reload fallback:
#   sudo launchctl bootout system/com.ultraleap.tracking.service && \
#   sudo launchctl bootstrap system /Library/LaunchDaemons/com.ultraleap.tracking.service.plist

# Verify the data path is back (no hand required — healthy service streams frames):
cargo run -p wc-core --example leap_recovery_probe --features hand-tracking-gestures
```

Physical USB replug remains the known-good fallback if rung 3 fails (it forces the same device-session rebuild). The GUI Ultraleap Control Panel may also expose a "restart service" action via its own vendor helper — the user-facing equivalent of rung 3.

---

## Goal

Unattended, reliable self-heal of the Leap dependency over 12+ hour runs, **without compromising machine security** (no blanket sudo / `NOPASSWD: ALL`). Detection decoupled from authority; recovery least-privilege and idempotent; bounded retries; hardware backstop.

## Core architecture: invert ownership

Do **not** make WaveConductor a privileged service-restarter. Let the **OS init/service manager supervise the dependency**, and make our app a **health reporter** that requests recovery through a *narrow, audited* channel. Restarting a peer service from inside the app is precisely what forces broad privilege; supervision keeps the app unprivileged.

## Detection (in-app, no privilege)

We already have the signal: the provider stamps `last_tracking_instant` on every tracking event and flips `TrackingFlow::NotStreaming` after `STALE_FRAME_THRESHOLD` (1 s) without frames. Because a healthy un-paused service streams continuously *even with no hand present*, "no frames while we believe we're un-paused" is an unambiguous wedge signal. Corroborate with leaprs `EventRef::ConnectionLost`. A watchdog should debounce (require sustained absence) to avoid acting on a transient blip, and gate on "we expect streaming" so intentional pauses don't false-trip.

## Recovery escalation ladder (cheapest / least-privilege first)

| Rung | Action | Privilege | Reachable on |
| ---- | ------ | --------- | ------------ |
| 0 | Observe / debounce (heartbeat + `ConnectionLost`) | none | all |
| 1 | **Client reconnect** — provider `stop()` + `start()` | none (in-app) | all desktop |
| 2 | **USB device reset** — re-enumerate the Leap | one device node, OS-scoped | Linux (sysfs `authorized`/`usbreset`, udev rule); Windows (`pnputil`/`devcon`/CM API); macOS (no clean per-device reset) |
| 2b | **USB VBUS power-cycle** — `uhubctl -a cycle` | device-node (udev) + PPPS-capable hub hardware | Linux + capable hub |
| 3 | **Restart the tracking service** | OS-scoped service authority | Linux (polkit action on the exact unit); Windows (SCM ACL on the exact service); macOS (SMAppService one-verb helper, or narrow sudoers on dev) |
| 4 | **Reboot** | systemd `StartLimitAction` / hardware watchdog / OS scheduler | Linux clean; Windows via SCM recovery; macOS helper |

**Settled (2026-06-03):** rung 1 (client reconnect / `set_pause(false)`) does **not** clear a service-level wedge — tested live with `leap_recovery_probe` (0 frames before *and* after, fresh client connected). On macOS rung 2 is unavailable (no clean per-device USB reset), so macOS recovery escalates **rung 0 → rung 1 (cheap, but expected to fail the daemon-freeze case) → rung 3 (privileged restart)**. The software analog of the confirmed human replug is therefore the **service restart**, not a USB reset. **Still open:** does rung 2 (USB reset) clear it on Linux/Windows — if so it's a no-privilege path those OSes get and macOS doesn't.

## Per-OS least-privilege mechanisms

### Linux
- **polkit rule** (`/etc/polkit-1/rules.d/`) granting *only* `restart` of *only* the Ultraleap unit to *only* the kiosk user, keyed on `org.freedesktop.systemd1.manage-units` + `action.lookup("unit")` + verb. Strictly better than sudoers (structured action+unit+verb, not a brittle command string). Then `systemctl restart <unit>` with no password.
- **udev rule** granting the kiosk user write to the specific Leap device node for USB reset (rung 2) — no sudo, no service restart.
- **systemd** `Restart=always` + `StartLimitBurst`/`StartLimitIntervalSec` (bound thrash) + `StartLimitAction=reboot` (last resort) as a drop-in over the vendor unit. **Caveat:** `Restart=`/`WatchdogSec` only catch process *crashes* / missed self-heartbeats — they will **not** catch our "alive but frozen" wedge, so the app-heartbeat path is mandatory.
- **Hardware watchdog** (`RuntimeWatchdogSec` in `system.conf`) → auto-reboot if the box itself hangs.
- Unit name to confirm on-device: likely `ultraleap-hand-tracking-service.service`.

### macOS
- **Automation / AppleEvents ("Allow this app to control other applications") does NOT apply** — it governs Apple Events to *scriptable apps*, not launchd daemons. It cannot restart `com.ultraleap.tracking.service` (a system LaunchDaemon, `libtrack_server` as root, parent launchd). Rule it out.
- **`SMAppService` (macOS 13+) privileged helper** exposing a *single* XPC verb ("restart-leap") — the correct deployment mechanism. The pre-13 path is `SMJobBless` + `/Library/PrivilegedHelperTools` + `AuthorizationRef`.
- **Dev-box interim:** a narrow `sudoers.d` line scoped to the exact `launchctl kickstart -k system/com.ultraleap.tracking.service` (run by Madison, or one-time rule). **Caveat:** macOS 14.4 restricts `kickstart -k` for Apple *critical* daemons; the Ultraleap daemon is third-party so should be unaffected — verify on-device; fall back to `bootout`+`bootstrap` or `launchctl kill`.
- No clean per-device USB reset analog on macOS (rung 2 is effectively Linux/Windows-only).

### Windows (now first-class — needs its own research pass)
- **Service Control Manager (SCM)** is the analog of systemd. The Ultraleap tracking service is a Windows Service. Least-privilege grant: set a **per-service security descriptor (DACL)** granting the kiosk user `SERVICE_START|SERVICE_STOP` on *only* that service (`sc.exe sdset` / `ServiceSecurity`), then restart via the SCM API with no admin token. This is the Windows analog of the polkit rule and must be researched/verified.
- **USB reset** via `pnputil /restart-device`, `devcon`, or the CfgMgr/SetupAPI `CM_Query_And_Remove_SubTree` + re-enable.
- **SCM service recovery actions** (restart-on-failure) are the `Restart=`-equivalent backstop (but, like systemd, only catch process failure, not a frozen-but-alive service).
- **Open:** confirm the exact Windows service name and the cleanest least-privilege SCM-ACL grant; this OS was not in the original research and needs a dedicated pass.

## What NOT to build

- A **general-purpose always-on privileged supervisor** that "talks to both apps and holds root." On Linux it reinvents systemd with more attack surface. The only legitimate custom-privileged form is a **one-verb helper** (the macOS `SMAppService` shape) — and on Linux/Windows the OS authorization (polkit / SCM ACL) makes even that unnecessary.

## Anti-patterns

- `NOPASSWD: ALL` or any broad/wildcarded sudoers grant.
- A sudoers rule that allows `systemctl *` or `systemctl restart *` (lets a compromised app stop sshd/firewall).
- A **setuid-root** wrapper (any bug = local root; bypasses the audited path).
- Relying on the macOS Automation permission to control a daemon.
- Assuming systemd `Restart=`/`WatchdogSec` (or Windows SCM recovery) will catch the **frozen-but-alive** wedge — they won't.
- Assuming `launchctl kickstart -k` always works on modern macOS.
- Acting on a single missed frame (debounce + corroborate first).
- Running `uhubctl`/USB-reset as root via sudo when a udev/device-ACL grant suffices.

## Security posture (recommended approach)

Privilege granted is bound to a *structured action + named target + specific verb + specific subject*, enforced by the OS's own authorization engine (polkit / SCM-ACL / SMAppService-XPC / udev), and auditable. A compromised WaveConductor process gains, at worst, the ability to bounce its own hand-tracking dependency or reset its own camera — a trivial blast radius versus full root. Rate-limit and log every recovery action; keep the rules under config management.

## Open questions / on-device verification

1. **Replicate the wedge deterministically.** Partial progress (2026-06-03): a wedge was *observed* again — but **spontaneously, with WaveConductor not running** — which points away from our duty cycle and toward GPU/concurrency contention (hypothesis (a)). We still lack a *deterministic trigger* we can fire on demand for recovery testing. Next: induce contention on purpose (run a synthetic GPU load alongside the Leap, no WaveConductor) and see if it wedges repeatably; only then re-test the duty cycle's contribution on top. Remaining hypotheses to rule in/out: (b) the duty cycle's live polling pattern vs the harness's tight polling; (c) connect/disconnect or pause churn hitting the documented memory leak.
2. **Client reconnect (rung 1): answered — does NOT clear a service-level wedge** (live test, `leap_recovery_probe`). Does **USB reset** (rung 2) clear it on Linux/Windows? (No macOS analog.) Does rung 3 (privileged restart) reliably clear it without a physical replug? — verify on the next live wedge using the operator runbook above.
3. Exact service/unit names per OS; macOS 14.4 `kickstart -k` behavior for the third-party daemon.
4. Windows least-privilege SCM-ACL grant + USB-reset path (dedicated research pass).
5. Hardware-watchdog availability on the chosen box(es); PPPS support on any hub bought for rung 2b.

## Sources

- Ultraleap — Known Issues (Gemini): https://support.ultraleap.com/hc/en-us/articles/4412486302353-Known-Issues-Gemini
- Ultraleap — Troubleshooting Guide V5 (Gemini): https://support.ultraleap.com/hc/en-us/articles/4406124780177-Troubleshooting-Guide-V5-Gemini
- Ultraleap community — "Gemini Ultraleap service constantly stopping": https://d2beseu6pw5d2t.cloudfront.net/t/gemini-ultraleap-service-constantly-stopping/16361/14
- systemd watchdog architecture (Poettering): http://0pointer.de/blog/projects/watchdog.html
- polkit for systemd unit control: https://www.baeldung.com/linux/systemd-service-restart-specific-user · https://wiki.debian.org/PolicyKit
- USB reset on Linux (sysfs `authorized`, `usbreset`, controller unbind): https://www.baeldung.com/linux/usb-device-reset-cli · https://blog.wesleyac.com/posts/linux-reset-usb
- uhubctl (per-port power, udev, PPPS): https://github.com/mvp/uhubctl/blob/master/README.md
- macOS `SMAppService` privileged helper: https://theevilbit.github.io/posts/smappservice/ · https://github.com/trilemma-dev/SwiftAuthorizationSample
- macOS TCC / AppleEvents scope: https://scriptingosx.com/2020/09/avoiding-applescript-security-and-privacy-requests/
- macOS 14.4 `launchctl kickstart -k` restriction: https://www.kevinmcox.com/2024/03/changes-to-launchctl-kickstart-in-macos-14-4/
- leaprs (events, no auto-reconnect): https://docs.rs/leaprs/latest/leaprs/ · https://github.com/plule/leaprs
