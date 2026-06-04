# Leap Wedge Detector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect when the Ultraleap service has wedged (alive-but-frozen: control path responds, frame stream dead) and surface it via the existing status LED, a log line, and an edge-triggered Bevy message — detection only, no recovery.

**Architecture:** A pure, always-compiled `LeapWedgeDetector` state machine (modeled on `input/idle_pause.rs`) owned by `LeaprsProvider`, fed `(now, expecting_streaming, is_streaming)` each `poll()`. It latches a `wedged` flag onto `ProviderStatus`, which `primary()` maps to a new `PrimaryState::DeviceWedged`, driving the existing LED. A tiny `surface_leap_wedge` system edge-detects that state and emits `LeapWedgeChanged` + a `tracing` line.

**Tech Stack:** Rust, Bevy 0.18 (ECS, `Message`/`Messages`, `Local`, `Res`/`ResMut`), `leaprs` (native Leap, feature `hand-tracking-gestures`).

**Spec:** [`../specs/2026-06-03-leap-wedge-detector-design.md`](../specs/2026-06-03-leap-wedge-detector-design.md)

---

## File Structure

| File | Responsibility | Task |
| --- | --- | --- |
| `crates/wc-core/src/input/wedge.rs` (new) | Pure `LeapWedgeDetector` + `WEDGE_THRESHOLD` + `WedgeTransition`. No Bevy, no `leaprs`. Unit-tested. | 1 |
| `crates/wc-core/src/input/mod.rs` | `pub mod wedge;`; register `LeapWedgeChanged`; insert `surface_leap_wedge` into the `PreUpdate` chain. | 1, 4 |
| `crates/wc-core/src/input/state.rs` | `ProviderStatus::wedged` field; `PrimaryState::DeviceWedged`; `primary()` rule; table tests. | 2 |
| `crates/wc-core/src/ui/buttons.rs` | One LED color/tooltip arm for `DeviceWedged` (forced by the exhaustive match). | 2 |
| `crates/wc-core/src/input/providers/mock.rs` | Add `wedged: false` to its `status()` literal; mock-never-wedges test. | 2 |
| `crates/wc-core/src/input/providers/leap_native.rs` | Own the detector + a `paused` field; drive detection in `poll()`; reset on `start()`/`stop()`; record intent in `set_paused`. Tests. | 3 |
| `crates/wc-core/src/input/systems.rs` | `LeapWedgeChanged` message + `surface_leap_wedge` system + integration tests. | 4 |

**Field/type names used throughout (keep consistent):** `LeapWedgeDetector`, `WEDGE_THRESHOLD`, `WedgeTransition::{None, Entered, Cleared}`, `LeapWedgeDetector::poll(now, expecting_streaming, is_streaming) -> WedgeTransition`, `LeapWedgeDetector::is_wedged() -> bool`, `LeapWedgeDetector::reset()`, `ProviderStatus.wedged: bool`, `PrimaryState::DeviceWedged`, `LeaprsProvider.wedge: LeapWedgeDetector`, `LeaprsProvider.paused: bool`, `LeapWedgeChanged { wedged: bool, at: Duration }`, `surface_leap_wedge`.

---

## Task 1: Pure `LeapWedgeDetector`

**Files:**
- Create: `crates/wc-core/src/input/wedge.rs`
- Modify: `crates/wc-core/src/input/mod.rs` (add module declaration)

- [ ] **Step 1: Create `wedge.rs` with types, stubbed logic, and the full test module**

Create `crates/wc-core/src/input/wedge.rs`:

```rust
//! Wedge detector for the Ultraleap tracking service (roadmap `leap-deep-idle-state`).
//!
//! Pure logic — no Bevy, no `leaprs`, always compiled — so it unit-tests without
//! hardware or the `hand-tracking-gestures` feature. The Bevy wiring that feeds it
//! and surfaces its verdict lives in [`crate::input::providers::leap_native`] and
//! [`crate::input::systems`].
//!
//! A "wedge" is the Ultraleap service alive-but-frozen: its control path still
//! responds (`set_pause` keeps ack'ing) but the frame stream is dead. The provider
//! heartbeat already degrades `Streaming → NotStreaming` after ~1 s of silence; this
//! is the next debounce stage that distinguishes a real wedge from a benign pause:
//! sustained not-streaming **while we expect streaming** for [`WEDGE_THRESHOLD`].

use std::time::Duration;

/// Sustained not-streaming-while-expecting, beyond the provider's ~1 s
/// `STALE_FRAME_THRESHOLD`, before we call it a wedge. The heartbeat already
/// spends ~1 s declaring `NotStreaming`, so this adds ~2 s of confirmed silence —
/// long enough to ride out a GPU-contention hitch or a duty-cycle gap, short
/// enough to surface promptly (total LED latency ≈ 1 s + `WEDGE_THRESHOLD` ≈ 4 s).
pub const WEDGE_THRESHOLD: Duration = Duration::from_secs(3);

/// Edge produced by [`LeapWedgeDetector::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WedgeTransition {
    /// No edge this tick.
    None,
    /// Just entered a wedge (silence crossed [`WEDGE_THRESHOLD`]).
    Entered,
    /// Just recovered (frames resumed, or we stopped expecting them).
    Cleared,
}

/// Debounced wedge detector. Owned by the native provider; advanced once per
/// `poll()`. Pure: all state is the timer stamp + the latched verdict.
#[derive(Clone, Copy, Debug, Default)]
pub struct LeapWedgeDetector {
    /// Monotonic time the current not-streaming-while-expecting run began.
    not_streaming_since: Option<Duration>,
    /// Latched verdict: are we currently wedged?
    wedged: bool,
}

impl LeapWedgeDetector {
    /// Advance against the monotonic clock.
    ///
    /// - `expecting_streaming`: the service *should* be producing frames now
    ///   (connected, device attached, has streamed before, not intentionally paused).
    /// - `is_streaming`: frames are currently flowing.
    ///
    /// Returns the edge for this tick; [`Self::is_wedged`] exposes the latched state.
    pub fn poll(
        &mut self,
        now: Duration,
        expecting_streaming: bool,
        is_streaming: bool,
    ) -> WedgeTransition {
        // Silence isn't suspicious (paused / never-streamed / device gone) or
        // frames are flowing → clear the timer; report recovery if we were wedged.
        if !expecting_streaming || is_streaming {
            self.not_streaming_since = None;
            return if std::mem::take(&mut self.wedged) {
                WedgeTransition::Cleared
            } else {
                WedgeTransition::None
            };
        }
        // expecting_streaming && !is_streaming: arm on the first such tick, latch
        // once silence exceeds the threshold.
        let since = *self.not_streaming_since.get_or_insert(now);
        if !self.wedged && now.saturating_sub(since) >= WEDGE_THRESHOLD {
            self.wedged = true;
            WedgeTransition::Entered
        } else {
            WedgeTransition::None
        }
    }

    /// The latched verdict. Mirrored onto `ProviderStatus::wedged` by the provider.
    #[must_use]
    pub fn is_wedged(&self) -> bool {
        self.wedged
    }

    /// Reset to the default (not wedged, timer disarmed). Called on provider
    /// `start()`/`stop()` so a reconnect doesn't inherit stale state.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn threshold_exceeds_heartbeat() {
        // Must debounce on a *confirmed* NotStreaming (the provider's ~1 s
        // STALE_FRAME_THRESHOLD), not race the heartbeat.
        assert!(WEDGE_THRESHOLD >= Duration::from_secs(1));
    }

    #[test]
    fn benign_not_streaming_below_threshold() {
        let mut d = LeapWedgeDetector::default();
        assert_eq!(d.poll(Duration::ZERO, true, false), WedgeTransition::None);
        assert_eq!(
            d.poll(WEDGE_THRESHOLD - at(1), true, false),
            WedgeTransition::None
        );
        assert!(!d.is_wedged());
    }

    #[test]
    fn intentional_pause_never_wedges() {
        let mut d = LeapWedgeDetector::default();
        for ms in [0, 5_000, 10_000] {
            assert_eq!(d.poll(at(ms), false, false), WedgeTransition::None);
        }
        assert!(!d.is_wedged());
    }

    #[test]
    fn real_wedge_after_threshold_is_edge_once() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false); // arm
        assert_eq!(d.poll(WEDGE_THRESHOLD, true, false), WedgeTransition::Entered);
        assert!(d.is_wedged());
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(500), true, false),
            WedgeTransition::None
        );
        assert!(d.is_wedged());
    }

    #[test]
    fn recovery_clears() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(100), true, true),
            WedgeTransition::Cleared
        );
        assert!(!d.is_wedged());
    }

    #[test]
    fn pause_during_wedge_clears() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(100), false, false),
            WedgeTransition::Cleared
        );
        assert!(!d.is_wedged());
    }

    #[test]
    fn re_wedge_after_recovery() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        d.poll(WEDGE_THRESHOLD + at(100), true, true); // Cleared
        d.poll(WEDGE_THRESHOLD + at(200), true, false); // re-arm
        assert_eq!(
            d.poll(WEDGE_THRESHOLD + at(200) + WEDGE_THRESHOLD, true, false),
            WedgeTransition::Entered
        );
    }

    #[test]
    fn reset_disarms() {
        let mut d = LeapWedgeDetector::default();
        d.poll(Duration::ZERO, true, false);
        d.poll(WEDGE_THRESHOLD, true, false); // Entered
        d.reset();
        assert!(!d.is_wedged());
        assert_eq!(d.poll(Duration::ZERO, true, false), WedgeTransition::None);
    }
}
```

Add the module to `crates/wc-core/src/input/mod.rs` — insert after the `pub mod systems;` line (line 58), keeping the list alphabetical:

```rust
pub mod systems;
pub mod wedge;
```

- [ ] **Step 2: Run the tests**

Run: `cargo nextest run -p wc-core -E 'test(wedge)'`
Expected: PASS — 8 tests in `input::wedge::tests`. (The logic is implemented in Step 1; this confirms it.)

- [ ] **Step 3: Verify clippy is clean for the new file**

Run: `cargo clippy -p wc-core --all-targets --features hand-tracking-gestures -- -D warnings`
Expected: no errors. (Watch for `as_conversions`, `unused`, doc-backtick lints — there should be none.)

- [ ] **Step 4: Commit**

```bash
git add crates/wc-core/src/input/wedge.rs crates/wc-core/src/input/mod.rs
git commit -m "input/leap: pure LeapWedgeDetector debounce state machine"
```

---

## Task 2: `wedged` status axis + `PrimaryState::DeviceWedged` + LED

**Files:**
- Modify: `crates/wc-core/src/input/state.rs` (struct field, enum variant, `primary()` rule, fix 3 test literals, 2 new tests)
- Modify: `crates/wc-core/src/ui/buttons.rs` (one LED arm)
- Modify: `crates/wc-core/src/input/providers/mock.rs` (fix `status()` literal, 1 new test)

- [ ] **Step 1: Add the `wedged` field to `ProviderStatus`**

In `crates/wc-core/src/input/state.rs`, in `pub struct ProviderStatus` (around line 261-272), add a field after `service_health`:

```rust
    /// Service-side health conditions.
    pub service_health: ServiceHealth,
    /// `true` when the service is wedged: attached + expected-to-stream but the
    /// frame stream is sustained-dead. Set only by the native provider's
    /// `LeapWedgeDetector`; `primary()` maps it to `PrimaryState::DeviceWedged`.
    pub wedged: bool,
```

- [ ] **Step 2: Fix the full-literal construction sites broken by the new field**

`ProviderStatus` is built with `..Default::default()` in most places, but four sites enumerate every field and will now fail to compile. Add `wedged: false,` to each.

In `crates/wc-core/src/input/state.rs`, the three test literals (the `provider_status_primary_streaming_healthy`, `…_smudged_is_degraded`, and `…_service_health_low_fps_is_degraded` tests — around lines 425, 440, 474). Each currently ends with `service_health: …,` and a closing `};`. Add `wedged: false,` before the closing brace, e.g.:

```rust
            service_health: ServiceHealth::empty(),
            wedged: false,
        };
```

In `crates/wc-core/src/input/providers/mock.rs`, the `status()` literal (around line 117-126):

```rust
            service_health: ServiceHealth::empty(),
            wedged: false,
        }
```

- [ ] **Step 3: Add the `DeviceWedged` variant and the `primary()` rule**

In `crates/wc-core/src/input/state.rs`, add to `pub enum PrimaryState` (after `DeviceAttached`, around line 244):

```rust
    /// Device attached and expected to stream, but the frame stream is
    /// sustained-dead — the service is wedged (alive-but-frozen).
    DeviceWedged,
```

In `ProviderStatus::primary()`, insert the rule **after** the `Streaming { .. }` branch returns (after the `return PrimaryState::Streaming;` block, ~line 326) and **before** `// Rule 6`:

```rust
            return PrimaryState::Streaming;
        }

        // Rule 5.5 — attached + expected-to-stream but stream sustained-dead.
        // Below DeviceFailed/Disconnected (a real failure is more actionable),
        // above benign DeviceAttached (the whole point: distinguish a wedge).
        if self.wedged && matches!(self.device, DevicePresence::Attached) {
            return PrimaryState::DeviceWedged;
        }

        // Rule 6
```

- [ ] **Step 4: Add the LED color/tooltip arm**

In `crates/wc-core/src/ui/buttons.rs`, in `leap_led_color_and_tooltip` (the `match state` around line 524-539), add an arm before `PrimaryState::DeviceFailed`:

```rust
        PrimaryState::DeviceWedged => (
            Color32::from_rgb(0xe6, 0x7e, 0x22),
            "Tracking frozen (service wedged)",
        ),
        PrimaryState::DeviceFailed => (Color32::from_rgb(0xc0, 0x39, 0x2b), "Device error"),
```

- [ ] **Step 5: Write the new tests**

In `crates/wc-core/src/input/state.rs` test module (alongside the other `provider_status_primary_*` tests, ~line 493), add:

```rust
    #[test]
    fn provider_status_primary_wedged_attached_is_device_wedged() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Attached,
            streaming: TrackingFlow::NotStreaming,
            wedged: true,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::DeviceWedged);
    }

    #[test]
    fn provider_status_primary_device_failed_outranks_wedged() {
        let s = ProviderStatus {
            service: ServiceConnection::Connected,
            device: DevicePresence::Failed,
            wedged: true,
            ..ProviderStatus::default()
        };
        assert_eq!(s.primary(), PrimaryState::DeviceFailed);
    }
```

In `crates/wc-core/src/input/providers/mock.rs` test module (~line 168, after `start_transitions_to_streaming`), add:

```rust
    #[test]
    fn mock_never_reports_wedged() {
        let mut provider = MockProvider::with_frames([]);
        provider.start().expect("mock provider start cannot fail");
        assert!(!provider.status().wedged);
        assert_ne!(provider.status().primary(), PrimaryState::DeviceWedged);
    }
```

- [ ] **Step 6: Run the tests and clippy**

Run: `cargo nextest run -p wc-core --features hand-tracking-gestures -E 'test(primary) or test(wedged) or test(mock_never)'`
Expected: PASS, including the two new `primary` tests and `mock_never_reports_wedged`.

Run: `cargo clippy -p wc-core --all-targets --features hand-tracking-gestures -- -D warnings`
Expected: no errors. (The exhaustive LED match now compiles because Step 4 added the arm.)

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/src/input/state.rs crates/wc-core/src/ui/buttons.rs crates/wc-core/src/input/providers/mock.rs
git commit -m "input/leap: PrimaryState::DeviceWedged axis + LED arm"
```

---

## Task 3: Wire the detector into `LeaprsProvider`

**Files:**
- Modify: `crates/wc-core/src/input/providers/leap_native.rs` (imports, two fields, `start`/`stop`/`set_paused`/`poll`, two tests)

All changes here are behind `feature = "hand-tracking-gestures"`.

- [ ] **Step 1: Import the detector and add the two fields**

In `crates/wc-core/src/input/providers/leap_native.rs`, add to the imports near the top (with the other `use crate::input::…` lines):

```rust
use crate::input::wedge::LeapWedgeDetector;
```

Add two fields to `pub struct LeaprsProvider` (after `pause_policy_applied`, around line 93). The struct derives `Default`, and both fields default correctly (`false` / detector default):

```rust
    pause_policy_applied: bool,
    /// Our last requested pause state. `set_paused` records intent here so the
    /// wedge detector can gate on "we expect streaming" (we don't expect frames
    /// while we've asked the service to pause).
    paused: bool,
    /// Debounced wedge detector (frozen-but-alive service). Advanced each `poll`.
    wedge: LeapWedgeDetector,
```

- [ ] **Step 2: Reset the new state on `start()` and `stop()`**

In `start()` (after `self.pause_policy_applied = false;`, ~line 119):

```rust
        self.background_policy_applied = false;
        self.pause_policy_applied = false;
        self.paused = false;
        self.wedge.reset();
```

In `stop()` (after `self.last_tracking_instant = None;`, ~line 132):

```rust
        self.connection = None;
        self.status = ProviderStatus::default();
        self.last_tracking_instant = None;
        self.paused = false;
        self.wedge.reset();
```

- [ ] **Step 3: Record pause intent in `set_paused`**

In `set_paused` (around line 323), add as the **first** line of the function body, before the `pause_policy_applied` guard — so intent is recorded even when the policy isn't granted yet:

```rust
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !self.pause_policy_applied {
```

- [ ] **Step 4: Drive the detector at the end of `poll()`**

In `poll()`, rename the unused `_now` parameter to `now` (signature line ~140: `fn poll(&mut self, now: Duration, out: &mut Messages<HandTrackingFrame>)`). Then, after the heartbeat `match self.status.streaming { … }` block (after line ~199), add:

```rust
        // Wedge detection: a service that should be streaming but has gone
        // silent (control path alive, frames dead). Gated so an intentional
        // pause, or a cold start that never streamed, is not mistaken for a
        // wedge. `last_tracking_instant.is_some()` means "we have streamed at
        // least once", so a slow first-frame handshake reads as benign
        // DeviceAttached, not a wedge.
        let is_streaming = matches!(self.status.streaming, TrackingFlow::Streaming { .. });
        let expecting_streaming = matches!(self.status.service, ServiceConnection::Connected)
            && matches!(self.status.device, DevicePresence::Attached)
            && self.last_tracking_instant.is_some()
            && !self.paused;
        self.wedge.poll(now, expecting_streaming, is_streaming);
        self.status.wedged = self.wedge.is_wedged();
```

(`ServiceConnection`, `DevicePresence`, `TrackingFlow` are already imported in this file — they're used by `dispatch_event` and the heartbeat.)

- [ ] **Step 5: Write the no-hardware provider tests**

In `crates/wc-core/src/input/providers/leap_native.rs`, in the existing `#[cfg(test)] mod tests` block (footer of the file), add:

```rust
    #[test]
    fn fresh_provider_is_not_wedged() {
        let provider = LeaprsProvider::default();
        assert!(!provider.status().wedged);
    }

    #[test]
    fn set_paused_records_intent_even_before_policy_granted() {
        // No connection / policy yet, but intent must still be recorded so the
        // wedge gate ("we expect streaming") is correct.
        let mut provider = LeaprsProvider::default();
        provider.set_paused(true);
        assert!(provider.paused);
        provider.set_paused(false);
        assert!(!provider.paused);
    }
```

- [ ] **Step 6: Run the tests and clippy**

Run: `cargo nextest run -p wc-core --features hand-tracking-gestures -E 'test(fresh_provider) or test(set_paused_records)'`
Expected: PASS.

Run: `cargo clippy -p wc-core --all-targets --features hand-tracking-gestures -- -D warnings`
Expected: no errors. (Confirms the renamed `now` is used and the new fields wire up cleanly.)

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/src/input/providers/leap_native.rs
git commit -m "input/leap: drive wedge detector from LeaprsProvider::poll"
```

---

## Task 4: `LeapWedgeChanged` message + `surface_leap_wedge` system

**Files:**
- Modify: `crates/wc-core/src/input/systems.rs` (message type, system, new test module)
- Modify: `crates/wc-core/src/input/mod.rs` (register message, insert system into chain)

- [ ] **Step 1: Add the imports, message type, and system to `systems.rs`**

In `crates/wc-core/src/input/systems.rs`, add to the imports near the top:

```rust
use std::time::Duration;
```

and ensure `PrimaryState` is reachable — add to the existing `use super::state::{…}` line (line 29):

```rust
use super::state::{FusedHand, FusedHandFrame, HandTrackingFrame, HandTrackingState, PrimaryState, MAX_HANDS};
```

Add the message type and system (place them after `poll_all_providers`, before `fuse_hand_frames`):

```rust
/// Edge-triggered change in the primary provider's wedge state. `wedged = true`
/// on entering a wedge, `false` on recovery. A future recovery increment
/// subscribes to this; for now it is consumed only for logging.
#[derive(Message, Debug, Clone, Copy)]
pub struct LeapWedgeChanged {
    /// `true` = just entered a wedge; `false` = just recovered.
    pub wedged: bool,
    /// Monotonic time (`Time::elapsed`) of the transition.
    pub at: Duration,
}

/// Surfaces wedge-state changes: edge-detects `PrimaryState::DeviceWedged` from
/// the primary provider's status and emits [`LeapWedgeChanged`] + a `tracing`
/// line on each transition. Reads the same `primary_status()` the LED reads, so
/// the LED, log, and message can't disagree.
///
/// Runs every `PreUpdate` tick; allocation-free (snapshot read + `Local` compare)
/// and logs edge-only, per the "zero work when idle" budget.
pub fn surface_leap_wedge(
    registry: Res<'_, ProviderRegistry>,
    time: Res<'_, Time>,
    mut wedge_changed: ResMut<'_, Messages<LeapWedgeChanged>>,
    mut was_wedged: Local<'_, bool>,
) {
    let wedged = matches!(
        registry.primary_status().primary(),
        PrimaryState::DeviceWedged
    );
    if wedged == *was_wedged {
        return;
    }
    *was_wedged = wedged;
    wedge_changed.write(LeapWedgeChanged {
        wedged,
        at: time.elapsed(),
    });
    if wedged {
        tracing::warn!(
            "Leap service wedged: device attached but frame stream dead — \
             recovery on macOS is a physical USB replug"
        );
    } else {
        tracing::info!("Leap service recovered: hand-tracking frames resumed");
    }
}
```

(`Res`, `ResMut`, `Local`, `Time`, `Messages`, `Message` come from `bevy::prelude::*`, already imported at line 23. `ProviderRegistry` is imported at line 28.)

- [ ] **Step 2: Register the message and insert the system in `mod.rs`**

In `crates/wc-core/src/input/mod.rs`, add the message registration after `.add_message::<HandGestureEvent>()` (line 90):

```rust
            .add_message::<HandGestureEvent>()
            .add_message::<systems::LeapWedgeChanged>()
```

Insert the system into the `PreUpdate` chain, right after `poll_all_providers` (around line 113):

```rust
                (
                    systems::poll_all_providers,
                    systems::surface_leap_wedge,
                    systems::fuse_hand_frames,
                    systems::sync_hand_entities,
                    systems::mirror_state_resource,
                    systems::detect_gestures,
                    pointer_merge_system,
                )
```

- [ ] **Step 3: Write the integration test**

Add a test module at the end of `crates/wc-core/src/input/systems.rs`:

```rust
#[cfg(test)]
mod wedge_surface_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use bevy::prelude::*;

    use super::{surface_leap_wedge, LeapWedgeChanged};
    use crate::input::provider::{
        HandTrackingProvider, ProviderId, ProviderRegistry, ProviderRole,
    };
    use crate::input::state::{
        DevicePresence, HandTrackingError, ProviderDiagnostics, ProviderStatus, ServiceConnection,
        TrackingFlow,
    };
    use crate::input::state::HandTrackingFrame;

    /// Test provider whose wedge state is flipped from the test via a shared flag.
    struct StubProvider {
        wedged: Arc<AtomicBool>,
    }

    impl HandTrackingProvider for StubProvider {
        fn start(&mut self) -> Result<(), HandTrackingError> {
            Ok(())
        }
        fn stop(&mut self) {}
        fn poll(&mut self, _now: Duration, _out: &mut Messages<HandTrackingFrame>) {}
        fn status(&self) -> ProviderStatus {
            ProviderStatus {
                service: ServiceConnection::Connected,
                device: DevicePresence::Attached,
                streaming: TrackingFlow::NotStreaming,
                wedged: self.wedged.load(Ordering::Relaxed),
                ..ProviderStatus::default()
            }
        }
        fn diagnostics(&self) -> ProviderDiagnostics {
            ProviderDiagnostics::default()
        }
    }

    fn app_with_stub(flag: Arc<AtomicBool>) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_message::<LeapWedgeChanged>();
        let mut registry = ProviderRegistry::default();
        registry.register(
            ProviderId::Leap,
            ProviderRole::Primary,
            Box::new(StubProvider { wedged: flag }),
        );
        app.insert_resource(registry);
        app.add_systems(Update, surface_leap_wedge);
        app
    }

    fn drain(app: &mut App) -> Vec<LeapWedgeChanged> {
        app.world_mut()
            .resource_mut::<Messages<LeapWedgeChanged>>()
            .drain()
            .collect()
    }

    #[test]
    fn emits_enter_then_clear_edges() {
        let flag = Arc::new(AtomicBool::new(true));
        let mut app = app_with_stub(flag.clone());

        app.update();
        let msgs = drain(&mut app);
        assert_eq!(msgs.len(), 1, "one enter edge");
        assert!(msgs[0].wedged);

        flag.store(false, Ordering::Relaxed);
        app.update();
        let msgs = drain(&mut app);
        assert_eq!(msgs.len(), 1, "one clear edge");
        assert!(!msgs[0].wedged);
    }

    #[test]
    fn no_edge_when_state_unchanged() {
        let flag = Arc::new(AtomicBool::new(false)); // never wedged
        let mut app = app_with_stub(flag);
        app.update();
        app.update();
        assert!(drain(&mut app).is_empty());
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p wc-core -E 'test(wedge_surface)'`
Expected: PASS — `emits_enter_then_clear_edges` and `no_edge_when_state_unchanged`.

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p wc-core --all-targets --features hand-tracking-gestures -- -D warnings`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/input/systems.rs crates/wc-core/src/input/mod.rs
git commit -m "input/leap: surface_leap_wedge system + LeapWedgeChanged message"
```

---

## Task 5: Full verification + status update

**Files:**
- Modify: `docs/superpowers/specs/2026-06-03-leap-wedge-detector-design.md` (status line)
- Modify: `docs/superpowers/roadmap.md` (note detection landed)

- [ ] **Step 1: Run the full CI gate suite**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```
Expected: all pass. (The `rustfmt.toml` nightly-feature warnings and ~29 pre-existing doc-link warnings are expected/harmless per AGENTS.md. `cargo doc` should not add *new* warnings.)

- [ ] **Step 2: Mark the spec implemented**

In `docs/superpowers/specs/2026-06-03-leap-wedge-detector-design.md`, change the status line:

```markdown
**Status:** Implemented 2026-06-03 (plan `docs/superpowers/plans/2026-06-03-leap-wedge-detector.md`). Detection half of `leap-deep-idle-state`.
```

- [ ] **Step 3: Note it on the roadmap**

In `docs/superpowers/roadmap.md`, under the `leap-deep-idle-state` item, add a bullet:

```markdown
- ✅ Detection landed: `PrimaryState::DeviceWedged` drives the status LED + a `LeapWedgeChanged` message after ~4 s of attached-but-frozen silence (`input/wedge.rs`, `surface_leap_wedge`). Recovery (privileged restart / USB reset / alerting) is the remaining, separate increment.
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-03-leap-wedge-detector-design.md docs/superpowers/roadmap.md
git commit -m "docs: mark Leap wedge-detector detection increment implemented"
```

---

## Notes for the implementer

- **No hardware needed.** Every test runs without a Leap or the device — the detector is pure, and the system test uses a stub provider. Don't try to exercise the real `LeaprsProvider::poll` wedge path in a test (it needs a live `leaprs::Connection`); its logic is the already-tested detector plus mechanical wiring.
- **Commit trailer:** end each commit message with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` (this repo's convention).
- **Keep it green at each commit.** Tasks are ordered so the workspace compiles after every task: Task 2 adds the enum variant *and* its forced LED arm together; nothing sets `wedged = true` in production until Task 3, and the system that reads it lands in Task 4.
- **AGENTS.md compliance baked in:** pure logic in `wedge.rs` with a `#[cfg(test)]` footer; `///` docs on every public item; `Duration` + `saturating_sub` (no numeric `as`); no `unwrap`/`expect` outside tests; one concept per file.
