# In-app Leap wedge detector — design

**Status:** Design (approved 2026-06-03). Implements the *detection* half of roadmap item `leap-deep-idle-state`.
**Scope:** **Detect + surface only.** No recovery actions in this increment (recovery is a separate, larger, per-OS effort — see the recovery design doc).
**Related:**
- Why/architecture of the wedge & recovery: [`2026-06-03-leap-service-recovery-design.md`](2026-06-03-leap-service-recovery-design.md)
- Operator troubleshooting: [`../../runbooks/leap-wedge-troubleshooting.md`](../../runbooks/leap-wedge-troubleshooting.md)

---

## Goal

Make the app *notice* when the Ultraleap service has wedged (alive-but-frozen: control path responds, frame stream is dead) and surface it through the **existing lower-left status LED**, a log line, and an edge-triggered Bevy event — so the kiosk stops silently sitting dead and an operator can act (on macOS: replug).

## The gap this closes

During a Class-A wedge the device stays `DevicePresence::Attached` but `TrackingFlow` flips to `NotStreaming`, so `ProviderStatus::primary()` returns `PrimaryState::DeviceAttached` — **identical to a benign "attached but momentarily not streaming" moment** (startup, an intentional pause). The LED therefore cannot tell a wedge from a normal lull. The detector closes that gap by distinguishing **attached + we-expect-streaming + sustained-not-streaming** → a new, distinct wedged state.

(The other class — "no leap detected" — already surfaces as `ServiceOnly`/`Disconnected`, so it is out of scope here.)

## Architecture

A **pure state-machine struct owned by the native provider**, modeled one-for-one on the existing `LeapIdlePause` (`input/idle_pause.rs`): no Bevy, no `leaprs`, fed a monotonic `now: Duration` plus two booleans, fully unit-testable without hardware. It is the next debounce stage on the *same* heartbeat signal the provider already computes (the `Streaming → NotStreaming` degrade at `STALE_FRAME_THRESHOLD`). Its latched output rides on `ProviderStatus`, which `primary()` already maps to the LED — keeping the LED a **single source of truth**.

Chosen over (a) a separate app-level watchdog resource — which would give the LED two truth sources and force it to re-derive "attached + not streaming" from a snapshot — and (b) smearing time logic into the currently-stateless `primary()`. (Senior-engineer review, 2026-06-03.)

### Two corrections this design bakes in (from the code, not assumptions)

1. **There is no mirrored `Res<ProviderStatus>`.** The LED (`ui/buttons.rs::draw_leap_status_led`) and dev panel call `registry.primary_status()` **live each frame**. "Latch `wedged` onto `ProviderStatus`" means the struct the provider returns from `status()` (its `self.status` field) — not a Bevy resource.
2. **The provider does not currently record its own pause state.** `set_paused` is fire-and-forget. We add a `paused: bool` field to gate "we expect streaming." (Do **not** use `DeviceHealth::PAUSED` — that is device-reported, event-driven, and races our own `set_pause` call.)

## Components & where they live

| File | Change |
| --- | --- |
| `crates/wc-core/src/input/wedge.rs` | **New.** Pure `LeapWedgeDetector` + `WEDGE_THRESHOLD` + `WedgeTransition { None, Entered, Cleared }`. `Duration` math, `saturating_sub`, no `as`/`unwrap`. Unit tests at the file footer. |
| `crates/wc-core/src/input/state.rs` | Add `wedged: bool` to `ProviderStatus`; add `PrimaryState::DeviceWedged`; add the precedence rule in `primary()`. Extend the `primary()` table tests. |
| `crates/wc-core/src/input/providers/leap_native.rs` | Own a `LeapWedgeDetector`; add `paused: bool`; drive the detector in `poll()` (using the currently-unused `now` arg); reset detector + `paused` in `stop()`/`start()`; set `paused` in `set_paused` on success. |
| `crates/wc-core/src/input/systems.rs` | **New system** `surface_leap_wedge`: reads `registry.primary_status()`, edge-detects via a `Local<Option<bool>>`, emits `LeapWedgeChanged` + `tracing::warn` (enter) / `info` (clear). |
| `crates/wc-core/src/input/mod.rs` | `pub mod wedge;`; `add_message::<LeapWedgeChanged>()`; insert `surface_leap_wedge` into the `PreUpdate` chain after `poll_all_providers`, in `InputSystems`. |
| `crates/wc-core/src/ui/buttons.rs` | One match arm: `PrimaryState::DeviceWedged => (orange-red, "Tracking frozen (service wedged)")`. The only change outside `input/`; forced by the exhaustive `PrimaryState` match — presentation is correctly a UI concern. |
| `crates/wc-core/src/input/providers/mock.rs`, `…/websocket.rs` | **No change.** `wedged` defaults `false` via `ProviderStatus`'s `Default`; the new `primary()` rule only fires when `wedged == true`, which only the native provider sets. (Add one assertion that mock never reports `DeviceWedged`.) |

## Data flow

```
LeaprsProvider::poll(now, …)
  └─ existing heartbeat: Streaming → NotStreaming after STALE_FRAME_THRESHOLD (1s)
  └─ expecting_streaming = service==Connected ∧ device==Attached ∧ !self.paused
  └─ LeapWedgeDetector::poll(now, expecting_streaming, is_streaming) → WedgeTransition
        ↳ self.status.wedged = detector.is_wedged()
ProviderStatus::primary()  ── reads self.status.wedged ──▶ PrimaryState::DeviceWedged
  ├─ ui/buttons.rs::draw_leap_status_led  (registry.primary_status().primary())  → LED color/tooltip
  └─ systems::surface_leap_wedge          (registry.primary_status())            → edge: LeapWedgeChanged + tracing
```

## The detector (pure logic)

```rust
/// Sustained not-streaming-while-expecting, BEYOND the 1s STALE_FRAME_THRESHOLD,
/// before we call it a wedge. The heartbeat already spends ~1s declaring
/// NotStreaming, so this adds ~2s of confirmed silence — long enough to ride out
/// a GPU-contention hitch or a duty-cycle gap, short enough to surface promptly
/// (total LED latency ≈ 1s + WEDGE_THRESHOLD ≈ 4s).
pub const WEDGE_THRESHOLD: Duration = Duration::from_secs(3);

pub enum WedgeTransition { None, Entered, Cleared }

pub struct LeapWedgeDetector { not_streaming_since: Option<Duration>, wedged: bool }
// poll(now, expecting_streaming, is_streaming) -> WedgeTransition
//   - !expecting_streaming  → clear timer; if was wedged → Cleared, else None
//   - is_streaming          → clear timer; if was wedged → Cleared, else None
//   - expecting ∧ !streaming → arm/keep timer; once held ≥ WEDGE_THRESHOLD and
//                              not already wedged → Entered; else None
```

`WEDGE_THRESHOLD > STALE_FRAME_THRESHOLD` is asserted in a test (mirrors `idle_pause.rs`'s invariant test).

**Two edge points, deliberately.** The detector exposes both a latched `is_wedged() -> bool` (drives `ProviderStatus::wedged`, hence the LED) and a `WedgeTransition` return (clean edge semantics for unit tests, and a future hook). The *event* (`LeapWedgeChanged`) is **not** emitted by the provider — `HandTrackingProvider::poll` only gets a `&mut Messages<HandTrackingFrame>`, not arbitrary app message writers — so `surface_leap_wedge` re-derives the enter/clear edge from `primary()` via a `Local`. The provider owns the *state*; the system owns the *app-facing event*. Both observe the same latched bool, so they cannot disagree.

## `primary()` precedence

Insert a new rule **between** the `Streaming { .. }` branch and the `DevicePresence::Attached → DeviceAttached` check — i.e. *below* `DeviceFailed` (rule 2) and the service-reachability checks (rule 3), *above* benign `DeviceAttached`:

```
// attached + expected-to-stream but stream sustained-dead (Class A wedge)
if self.wedged && matches!(self.device, DevicePresence::Attached) {
    return PrimaryState::DeviceWedged;
}
```

A genuine `DeviceFailed`/`Disconnected`/`ServiceMissing` therefore still wins (those are more actionable and accurate); a wedge only outranks the benign attached-but-idle state.

## Surfacing

- **LED:** new `DeviceWedged` arm → `Color32::from_rgb(0xe6, 0x7e, 0x22)` (orange-red, between blue `DeviceAttached` and red `DeviceFailed` in the severity ramp), tooltip `"Tracking frozen (service wedged)"`.
- **Event:** `#[derive(Message)] pub struct LeapWedgeChanged { pub wedged: bool, pub at: Duration }` (Bevy 0.18 `Message`/`Messages`, consistent with `HandTrackingFrame`/`HandGestureEvent`). A future recovery increment subscribes to this; nothing consumes it yet beyond logging.
- **Log:** `tracing::warn!` on enter (include device serial if available), `tracing::info!` on clear. Edge-only — never per-frame.

## Testing

Pure unit tests on `LeapWedgeDetector` (footer of `wedge.rs`, mirroring `idle_pause.rs`):

1. `benign_not_streaming_below_threshold` — expecting + not-streaming for < threshold → `None`, not wedged.
2. `intentional_pause_never_wedges` — `expecting=false` held arbitrarily long → `None`, timer never arms.
3. `real_wedge_after_threshold` — sustained past `WEDGE_THRESHOLD` → exactly one `Entered`, then `None` while held (edge-once).
4. `recovery_clears` — after wedged, a streaming poll → one `Cleared`, timer reset.
5. `pause_during_wedge_clears` — wedged, then `expecting` drops (duty-cycle pause) → `Cleared` (stop flagging a now-intentionally-paused service).
6. `re_wedge_after_recovery` — Entered → Cleared → Entered fires a fresh edge.
7. `threshold_beyond_stale` — assert `WEDGE_THRESHOLD > STALE_FRAME_THRESHOLD`.

Plus, in `state.rs`: a `primary()` table case asserting `wedged && DevicePresence::Failed` still yields `DeviceFailed`; and a mock assertion that it never reports `DeviceWedged`.

No hardware needed for any test.

## Limitations (accepted)

- **Latency:** ~4s from freeze to LED (1s heartbeat + 3s debounce). Tunable via `WEDGE_THRESHOLD`; cannot drop below the heartbeat.
- **Duty-cycle blind spot:** while the duty cycle is actively pausing (off by default), `expecting_streaming` is false most of the time, so a wedge induced *during* a paused phase is not flagged until the next expected-live window. Acceptable for detect-only.

## Risks for the implementer

- Keep `primary()` precedence correct: `DeviceFailed`/`Disconnected`/`ServiceMissing` must still win over `DeviceWedged` (covered by the table test).
- Reset the detector **and** `paused` in both `stop()` and `start()` so a restart doesn't inherit stale wedge/pause state.
- `surface_leap_wedge` runs every `PreUpdate` tick: keep it allocation-free (snapshot read + `Local` compare) and edge-only logging, per the AGENTS.md "zero work when idle" spirit.
- Pure struct uses `Duration` + `saturating_sub` only — no numeric `as`, no `unwrap`/`expect` outside tests; `///` docs on every public item; one concept per file.

## Out of scope (explicitly)

Recovery of any kind (client reconnect, USB reset, privileged service restart, alerting beyond the LED/log/event). The `LeapWedgeChanged` event is the seam a future recovery increment hooks into.
