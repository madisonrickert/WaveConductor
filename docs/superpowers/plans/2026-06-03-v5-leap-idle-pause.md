# Leap Idle-Pause (duty-cycled deep-idle Leap throttle) Implementation Plan

> **⚠ SUPERSEDED — 2026-06-03.** Phases 0–4 were executed, but live hardware testing **falsified the core duty-cycle approach**: rapid `set_pause` toggling wedged the Leap device. **Phase 2 (the `reset_on_interaction` fix) shipped and stands** (commit `e9831ab5`); the duty-cycle code is held on-branch. Work continues as the `leap-deep-idle-state` roadmap item — see `docs/superpowers/specs/2026-06-03-leap-service-recovery-design.md`. Retained as the historical execution record.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** During the screensaver (deep idle), throttle the Ultraleap tracking service to shed host CPU/heat while keeping it wake-able by a hand — a *duty-cycled pause* (pause most of the time, briefly un-pause every ~0.5 s to sample for a hand) rather than a flat pause that would make the touchless projector kiosk un-wakeable.

**Architecture:** A pure, hardware-free state machine (`LeapIdlePause`) decides Pause/Resume/Hold on a fixed period; a feature-gated Bevy system applies that decision to every registered Leap provider via the existing `set_all_leap_paused` chain. The screensaver present-rate throttle is taught to wake the app frequently enough to honour the duty cycle. A prerequisite bug is fixed first: empty Leap tracking frames must no longer count as user interaction, or the idle timer never reaches the screensaver while a Leap is connected.

**Tech Stack:** Rust, Bevy 0.18 (`SubStates`, `Res`/`ResMut`, `WinitSettings`/`UpdateMode`), `leaprs` 0.2.2 / LeapC (behind the `hand-tracking-gestures` feature), `cargo nextest`.

---

## Context & background (read before starting)

This plan closes roadmap slug `leap-idle-pause` (1.A) — carry-forward **CF #84**: `pause_leap_on_screensaver` / `resume_leap_on_active` (`crates/wc-core/src/input/providers/leap_native.rs:693,702`) are defined but **registered as systems nowhere**, so the Leap service runs at full power through the entire idle screensaver.

Planning uncovered two things that reshape the naive "register two systems" fix:

1. **The flat-pause design is wrong for the kiosk.** `set_paused(true)` makes the service emit *no frames* (`leap_native.rs:309`). But the wake path runs *through* hand frames: `reset_on_interaction` (`crates/wc-core/src/lifecycle/idle.rs:125`) returns the sub-state to `Active` when a `HandTrackingFrame` arrives, and `resume_leap_on_active` only un-pauses `OnEnter(Active)`. So a flat pause makes a hand unable to wake the install — fatal on a projector kiosk with no touchscreen, where touchless is the *primary* modality. The fix is a **duty cycle** (Madison-directed): keep the service paused, briefly un-pause every period **P** to sample. If a hand is present during a sample window, the normal wake path fires.

2. **Prerequisite bug — the screensaver never triggers while Leap streams.** `dispatch_event` writes a `HandTrackingFrame` on *every* `Tracking` event (`leap_native.rs:407-409`), and LeapC emits those continuously (with zero hands) whenever the service runs. `reset_on_interaction` counts *any* frame as interaction (`idle.rs:138`: `hand_tracking.read().count() > 0`). So a connected, running Leap resets the idle timer every frame → `SketchActivity` can never advance to `Idle`/`Screensaver`. This must be fixed first (Task 2), independent of the duty cycle, or nothing downstream is reachable. It also means an un-pause during a sample window would emit empty frames and instantly self-wake unless the fix lands.

### The math that sets the constants

Let **P** = duty-cycle period, **W** = un-paused sample window, **D** = P − W (paused gap), **L** = service resume latency (`set_pause(false)` → first usable frame).

- Worst-case wake latency ≈ **P** (a hand appearing right after a window waits the gap, then the next resume). It also equals the worst-case time a visitor must hold their hand for it to register.
- Leap-service CPU saved ≈ paused fraction = D/P = 1 − W/P, and W must be ≥ L (+ a frame to read), so saving ≈ **1 − L/P**.

Madison's targets: worst-case wake / required hold-time ≤ **0.5 s** → **P = 500 ms**. With P = 500 ms, saving ≈ 1 − 2L (L in seconds):

| Resume latency L | CPU saved | Verdict |
| ---------------- | --------- | ------- |
| 50 ms | ~90% | clear win |
| 150 ms | ~70% | solid |
| 250 ms | ~50% | marginal |
| ≥ 400 ms | ≤ 20% | B not worth it → fall back to "don't pause" |

**L is unknown and only measurable on hardware**, which is why Phase 1 is a measurement spike with an explicit go/no-go.

### Decision gate (end of Phase 1)

- **L < ~0.25 s** → proceed with Phases 3–5 (duty cycle).
- **L ≥ ~0.4 s** → the duty cycle can't beat leaving the service running. Ship **Phase 2 only** (the prerequisite bug fix — required regardless so the screensaver works at all with Leap connected), keep `resume_leap_on_active` wired as a safety net, and record in the roadmap that Leap idle-throttling is deferred pending the full-render soak (CF #87). Skip Phases 3–5.

Phase 2 (the interaction-reset fix) is **unconditional** — it ships on either branch.

### Conventions

- Commit messages use the repo's `area/scope: summary` subject style (see `git log`); include the standard `Co-Authored-By` trailer.
- CI gate commands (run before declaring a task done — see `AGENTS.md` → *Verifying changes*): `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features --workspace -- -D warnings`; `cargo nextest run --workspace --all-features`; `cargo test --doc --workspace`; `cargo xtask check-secrets`. The per-task "run the gates" steps mean these.
- All Leap code is behind `#[cfg(feature = "hand-tracking-gestures")]`; `--all-features` exercises it in CI. The example needs `--features hand-tracking-gestures` explicitly.

---

## File structure

| File | Responsibility | Change |
| ---- | -------------- | ------ |
| `crates/wc-core/examples/leap_pause_probe.rs` | Phase 1 hardware spike: measure resume latency L + prompt operator for paused-vs-running daemon CPU | Create |
| `crates/wc-core/Cargo.toml` | Register the example with `required-features` | Modify |
| `crates/wc-core/src/lifecycle/idle.rs` | `reset_on_interaction`: only hand-bearing frames count as interaction | Modify (`:115-142`) |
| `crates/wc-core/src/input/idle_pause.rs` | Pure duty-cycle state machine (`LeapIdlePause`, `DutyPhase`, `PauseAction`, consts) — no Bevy systems, no leaprs, always compiled, fully unit-tested | Create |
| `crates/wc-core/src/input/mod.rs` | `pub mod idle_pause;`; register the duty-cycle systems + `init_resource` (feature-gated) | Modify (`:47-57`, `:122-133`) |
| `crates/wc-core/src/input/providers/leap_native.rs` | `enter_leap_idle_pause` / `drive_leap_idle_pause` systems + `apply_pause_action`; remove superseded `pause_leap_on_screensaver`; keep `resume_leap_on_active` | Modify (`:668-704`) |
| `crates/wc-core/src/lifecycle/screensaver/mod.rs` | `apply_present_rate`: floor the reactive `wait` to the duty cycle's requested wake | Modify (`:184-212`) |
| `docs/superpowers/roadmap.md`, `docs/superpowers/next-plan-carry-forwards.md` | Ledger updates on completion | Modify |

---

## Phase 0 — Housekeeping

### Task 1: Confirm carry-forward scope

**Files:** none (review only)

- [ ] **Step 1: Note the carry-forwards this plan touches.** This plan implements **CF #84** (the unwired idle-pause systems). It also fixes the newly-discovered `reset_on_interaction` bug (Phase 2). **CF #79** (`MockProvider` doesn't override `as_any_mut`) is *related* but **not required**: the pause path downcasts to the concrete `LeaprsProvider`, which a mock can't stand in for, so behaviour-level pause tests are hardware-only and CI tests assert on the state machine + system wiring instead. Leave CF #79 open. No code change in this task.

---

## Phase 1 — Hardware spike (measure L; go/no-go)

> Operator (Madison) runs this with a Leap controller connected. The implementing agent writes the example; the human runs it and records the numbers.

### Task 2: `leap_pause_probe` example

**Files:**
- Create: `crates/wc-core/examples/leap_pause_probe.rs`
- Modify: `crates/wc-core/Cargo.toml`

- [ ] **Step 1: Register the example in `crates/wc-core/Cargo.toml`.**

Add to the end of the file (create an `[[example]]` table; keep it grouped with any existing example entries):

```toml
[[example]]
name = "leap_pause_probe"
required-features = ["hand-tracking-gestures"]
```

- [ ] **Step 2: Write the example.** Create `crates/wc-core/examples/leap_pause_probe.rs`:

```rust
//! Hardware spike for `leap-idle-pause` (Phase 1): measure the Ultraleap service
//! resume latency `L` — the time from `set_pause(false)` to the first frame that
//! carries a hand — and prompt the operator to read paused-vs-running daemon CPU.
//!
//! Run with a Leap controller connected and your hand held over the sensor:
//!
//! ```text
//! cargo run -p wc-core --example leap_pause_probe --features hand-tracking-gestures
//! ```
//!
//! This is a throwaway measurement harness, not production code: it drives the
//! real `LeaprsProvider` (best fidelity) directly, without a Bevy `App`.
#![cfg(feature = "hand-tracking-gestures")]
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use bevy::prelude::Messages;
use wc_core::input::provider::HandTrackingProvider;
use wc_core::input::providers::leap_native::LeaprsProvider;
use wc_core::input::state::HandTrackingFrame;

/// Poll once and return any frames produced since the last poll.
fn poll_frames(
    provider: &mut LeaprsProvider,
    msgs: &mut Messages<HandTrackingFrame>,
) -> Vec<HandTrackingFrame> {
    provider.poll(Duration::ZERO, msgs);
    msgs.drain().collect()
}

/// Keep the connection serviced for `dur`, polling every 5 ms. Returns how many
/// hand-bearing frames were seen (0 once the service is paused and quiesced).
fn pump(provider: &mut LeaprsProvider, msgs: &mut Messages<HandTrackingFrame>, dur: Duration) -> u32 {
    let start = Instant::now();
    let mut hands = 0u32;
    while start.elapsed() < dur {
        for frame in poll_frames(provider, msgs) {
            if !frame.hands.is_empty() {
                hands += 1;
            }
        }
        sleep(Duration::from_millis(5));
    }
    hands
}

fn main() {
    let mut provider = LeaprsProvider::default();
    provider.start().expect("LeaprsProvider::start failed — is a Leap connected?");
    let mut msgs = Messages::<HandTrackingFrame>::default();

    println!("Warming up (handshake + AllowPauseResume). Hold your hand over the sensor…");
    // ~4 s warm-up: lets the service connect and the pause policy arm, and
    // confirms a hand is actually present before we start timing.
    let seen = pump(&mut provider, &mut msgs, Duration::from_secs(4));
    assert!(seen > 0, "no hand-bearing frames during warm-up — keep your hand over the sensor");
    println!("Warm-up OK ({seen} hand frames). Starting latency measurement — keep your hand still.\n");

    // ── Latency measurement: 20 pause→resume cycles ──────────────────────────
    let mut samples: Vec<Duration> = Vec::with_capacity(20);
    for i in 1..=20 {
        provider.set_paused(true);
        // Let the service fully quiesce, then clear any backlog.
        sleep(Duration::from_millis(400));
        let _ = poll_frames(&mut provider, &mut msgs);

        let t0 = Instant::now();
        provider.set_paused(false);

        // Poll until a hand-bearing frame arrives (or give up after 2 s).
        let mut latency = None;
        let deadline = t0 + Duration::from_secs(2);
        while Instant::now() < deadline {
            if poll_frames(&mut provider, &mut msgs)
                .iter()
                .any(|f| !f.hands.is_empty())
            {
                latency = Some(t0.elapsed());
                break;
            }
            sleep(Duration::from_millis(1));
        }
        match latency {
            Some(l) => {
                println!("  cycle {i:>2}: L = {:>6.1} ms", l.as_secs_f64() * 1000.0);
                samples.push(l);
            }
            None => println!("  cycle {i:>2}: TIMEOUT (>2 s, no hand frame after resume)"),
        }
    }

    // ── Summary + saving estimate ────────────────────────────────────────────
    if !samples.is_empty() {
        samples.sort_unstable();
        let to_ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let median = samples[samples.len() / 2];
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let saving = (1.0 - median.as_secs_f64() / 0.5).clamp(0.0, 1.0) * 100.0;
        println!(
            "\nL: min {:.1} ms / median {:.1} ms / max {:.1} ms (n={})",
            to_ms(min), to_ms(median), to_ms(max), samples.len(),
        );
        println!("Estimated Leap-service CPU saved at P=500 ms ≈ {saving:.0}%");
        println!("Decision gate: median < 250 ms → proceed with duty cycle; ≥ 400 ms → fall back to \"don't pause\".\n");
    }

    // ── CPU windows: operator reads Activity Monitor / `top` ─────────────────
    println!("CPU CHECK — open Activity Monitor (or `top -o cpu`) and watch the Ultraleap service process.");
    println!(">>> SERVICE RUNNING for 30 s — note its %CPU now …");
    let running_hands = pump(&mut provider, &mut msgs, Duration::from_secs(30));
    provider.set_paused(true);
    println!("    (saw {running_hands} hand frames while running)");
    println!(">>> SERVICE PAUSED for 30 s — note its %CPU now (frames should stop) …");
    let paused_hands = pump(&mut provider, &mut msgs, Duration::from_secs(30));
    println!("    (saw {paused_hands} hand frames while paused — expect ~0)");

    provider.set_paused(false);
    provider.stop();
    println!("\nDone. Record: median L, running %CPU, paused %CPU. These set the Phase 4 tuning + the go/no-go.");
}
```

- [ ] **Step 3: Confirm it compiles.**

Run: `cargo build -p wc-core --example leap_pause_probe --features hand-tracking-gestures`
Expected: builds. If it fails with `LeaprsProvider`/`leap_native` not found, the module isn't publicly reachable — add `pub mod leap_native;` in `crates/wc-core/src/input/providers/mod.rs` (or a `pub use`) and rebuild. (`provider.poll` requires `HandTrackingProvider` in scope — already imported.)

- [ ] **Step 4: OPERATOR RUN (hardware).** With a Leap connected and a hand over the sensor:

Run: `cargo run -p wc-core --example leap_pause_probe --features hand-tracking-gestures`
Record: median L; running %CPU; paused %CPU. Apply the **decision gate** above.

- [ ] **Step 5: Commit the spike.**

```bash
git add crates/wc-core/examples/leap_pause_probe.rs crates/wc-core/Cargo.toml
git commit -m "input/leap: add leap_pause_probe hardware spike (CF #84, Phase 1)"
```

---

## Phase 2 — Prerequisite fix: empty Leap frames are not interaction (UNCONDITIONAL)

> Ships regardless of the Phase 1 decision — without it the screensaver never triggers while a Leap is connected.

### Task 3: `reset_on_interaction` ignores hand-less frames

**Files:**
- Modify: `crates/wc-core/src/lifecycle/idle.rs:115-142`
- Test: `crates/wc-core/tests/lifecycle.rs` (existing `reset_on_interaction` coverage lives here)

- [ ] **Step 1: Write the failing test.** Add to `crates/wc-core/tests/lifecycle.rs`. (Use the existing fixture `wc_core::input::synthetic::synthetic_hand_frame`, which yields a hand-bearing frame, and build an empty frame inline. Match the file's existing app-construction helper for registering `reset_on_interaction` + `InteractionTimer` + the `HandTrackingFrame` message; mirror the nearest existing test.)

```rust
#[test]
fn empty_leap_frames_do_not_reset_idle_timer() {
    use std::time::Duration;
    use bevy::prelude::*;
    use smallvec::SmallVec;
    use wc_core::input::provider::ProviderId;
    use wc_core::input::state::HandTrackingFrame;
    use wc_core::input::synthetic::synthetic_hand_frame;
    use wc_core::lifecycle::idle::{reset_on_interaction, InteractionTimer};

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<InteractionTimer>();
    app.add_message::<HandTrackingFrame>();
    app.add_systems(Update, reset_on_interaction);

    // Advance the clock so "last interaction" can be distinguished from ZERO.
    app.update();
    let baseline = app.world().resource::<InteractionTimer>().clone();

    // An EMPTY tracking frame (service running, no hand) must NOT reset.
    app.world_mut().write_message(HandTrackingFrame {
        provider: ProviderId::Leap,
        hands: SmallVec::new(),
        ..HandTrackingFrame::default()
    });
    app.update();
    assert_eq!(
        app.world().resource::<InteractionTimer>().last_interaction(),
        baseline.last_interaction(),
        "empty Leap frame should not count as interaction",
    );

    // A HAND-bearing frame MUST reset.
    let now = app.world().resource::<Time>().elapsed();
    app.world_mut().write_message(synthetic_hand_frame(now));
    app.update();
    assert!(
        app.world().resource::<InteractionTimer>().last_interaction()
            > baseline.last_interaction(),
        "hand-bearing frame should count as interaction",
    );
}
```

> If `HandTrackingFrame` has no `Default`, construct the empty frame with all fields explicit (see `crates/wc-core/src/input/state.rs:95`). If `InteractionTimer::last_interaction()` is not public, add `#[must_use] pub fn last_interaction(&self) -> Duration { self.last_interaction }` to the `impl InteractionTimer` block in `idle.rs` (it already exposes `idle_for`/`mark`).

- [ ] **Step 2: Run it; verify it fails.**

Run: `cargo nextest run -p wc-core --all-features empty_leap_frames_do_not_reset_idle_timer`
Expected: FAIL — the empty frame currently resets the timer (assertion 1 fails).

- [ ] **Step 3: Fix `reset_on_interaction`.** In `crates/wc-core/src/lifecycle/idle.rs`, change the hand-tracking term of `any_event` (`:134-138`) from counting any frame to counting only hand-bearing frames:

```rust
    let any_event = mouse_motion.read().count() > 0
        || mouse_buttons.read().count() > 0
        || keyboard.read().count() > 0
        || touch.read().count() > 0
        // A *hand* in the tracking volume is interaction; the empty tracking
        // frames a running-but-unoccupied Leap streams continuously are not —
        // otherwise the idle timer never reaches Screensaver while a Leap is
        // connected. `.filter().count()` (not `.any()`) so the reader cursor
        // fully drains (see the note above about peeking).
        || hand_tracking
            .read()
            .filter(|frame| !frame.hands.is_empty())
            .count()
            > 0;
```

Also update the doc comment at `:117-119` so it says a hand-*bearing* frame counts, not "any HandTrackingFrame arriving".

- [ ] **Step 4: Run the test; verify it passes.**

Run: `cargo nextest run -p wc-core --all-features empty_leap_frames_do_not_reset_idle_timer`
Expected: PASS.

- [ ] **Step 5: Run the gates, then commit.**

```bash
git add crates/wc-core/src/lifecycle/idle.rs crates/wc-core/tests/lifecycle.rs
git commit -m "lifecycle/idle: only hand-bearing Leap frames reset the idle timer"
```

---

## Phase 3 — Pure duty-cycle state machine (conditional on Phase 1 go)

### Task 4: `LeapIdlePause` state machine

**Files:**
- Create: `crates/wc-core/src/input/idle_pause.rs`
- Modify: `crates/wc-core/src/input/mod.rs:47-57` (add `pub mod idle_pause;`)

- [ ] **Step 1: Declare the module.** In `crates/wc-core/src/input/mod.rs`, add to the `pub mod` block (alphabetical, near `pub mod hand;`):

```rust
pub mod idle_pause;
```

- [ ] **Step 2: Write the module with its tests** (TDD: tests are inline and authoritative). Create `crates/wc-core/src/input/idle_pause.rs`:

```rust
//! Duty-cycle state machine for the deep-idle Leap throttle (roadmap
//! `leap-idle-pause`, CF #84).
//!
//! Pure logic — no Bevy systems, no `leaprs`, always compiled — so it unit-tests
//! without hardware or the `hand-tracking-gestures` feature. The Bevy wiring that
//! applies its decisions to the live provider lives in
//! [`crate::input::providers::leap_native`].
//!
//! ## Why a duty cycle, not a flat pause
//!
//! Flat-pausing the Leap service during the screensaver sheds the most CPU but
//! emits no frames, so a hand can never wake the install — fatal on the touchless
//! projector kiosk. Instead we keep the service paused for a gap `D`, then
//! un-pause for a short sample window `W` to look for a hand; period `P = W + D`.
//! Worst-case wake latency ≈ `P`; Leap-service CPU saved ≈ `1 − W/P`. See the
//! plan doc for the latency↔saving trade and how `W` is tuned to the measured
//! resume latency.

use std::time::Duration;

use bevy::prelude::Resource;

/// Worst-case wake latency / required hand-hold time. Madison-directed (0.5 s).
pub const IDLE_PAUSE_PERIOD: Duration = Duration::from_millis(500);

/// Un-paused sample window. **Placeholder** — tuned in Phase 4 to the measured
/// service resume latency `L` plus a small margin. Must stay `< IDLE_PAUSE_PERIOD`.
pub const IDLE_PAUSE_SAMPLE_WINDOW: Duration = Duration::from_millis(150);

/// Wake interval requested while sampling, so the app ticks fast enough to catch
/// the resume frame (the present-rate throttle floors its reactive `wait` to this).
pub const SAMPLE_POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Which half of the duty cycle we are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DutyPhase {
    /// Service paused, shedding CPU; waiting out the gap `D`.
    Paused,
    /// Service un-paused, sampling for a hand for the window `W`.
    Sampling,
}

/// What the caller should do to the Leap service this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseAction {
    /// No change since last tick.
    Hold,
    /// Pause the service now (entered the gap).
    Pause,
    /// Un-pause the service now (entered a sample window).
    Resume,
}

/// Duty-cycle clock for the deep-idle Leap throttle. Advanced once per tick while
/// the screensaver is showing; reset on screensaver entry.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LeapIdlePause {
    period: Duration,
    sample_window: Duration,
    phase: DutyPhase,
    /// Monotonic time (`Time::elapsed`) when the current phase began.
    phase_since: Duration,
}

impl Default for LeapIdlePause {
    fn default() -> Self {
        Self {
            period: IDLE_PAUSE_PERIOD,
            sample_window: IDLE_PAUSE_SAMPLE_WINDOW,
            phase: DutyPhase::Paused,
            phase_since: Duration::ZERO,
        }
    }
}

impl LeapIdlePause {
    /// The current duty phase.
    #[must_use]
    pub fn phase(&self) -> DutyPhase {
        self.phase
    }

    /// Paused gap `D = P − W`.
    #[must_use]
    fn gap(&self) -> Duration {
        self.period.saturating_sub(self.sample_window)
    }

    /// Begin the cycle paused (no visitor at deep-idle entry). Returns the action
    /// to apply now — always [`PauseAction::Pause`].
    pub fn reset_paused(&mut self, now: Duration) -> PauseAction {
        self.phase = DutyPhase::Paused;
        self.phase_since = now;
        PauseAction::Pause
    }

    /// Advance against the monotonic clock and return the pause action to apply.
    ///
    /// On a phase transition the phase clock resets to `now` (not the exact
    /// deadline), so the period drifts by at most one tick's latency per cycle —
    /// acceptable for a thermal duty cycle.
    pub fn advance(&mut self, now: Duration) -> PauseAction {
        let elapsed = now.saturating_sub(self.phase_since);
        match self.phase {
            DutyPhase::Paused => {
                if elapsed >= self.gap() {
                    self.phase = DutyPhase::Sampling;
                    self.phase_since = now;
                    PauseAction::Resume
                } else {
                    PauseAction::Hold
                }
            }
            DutyPhase::Sampling => {
                if elapsed >= self.sample_window {
                    self.phase = DutyPhase::Paused;
                    self.phase_since = now;
                    PauseAction::Pause
                } else {
                    PauseAction::Hold
                }
            }
        }
    }

    /// How soon the app should next wake to service the cycle. The screensaver
    /// present-rate throttle floors its reactive `wait` to this value so the gap
    /// ends on time and sample windows are polled fast enough to catch frames.
    #[must_use]
    pub fn requested_wake(&self, now: Duration) -> Duration {
        match self.phase {
            DutyPhase::Sampling => SAMPLE_POLL_INTERVAL,
            DutyPhase::Paused => {
                let elapsed = now.saturating_sub(self.phase_since);
                self.gap()
                    .saturating_sub(elapsed)
                    .max(Duration::from_millis(1))
            }
        }
    }

    /// Test seam: force the Sampling phase so a wiring test can prove
    /// `reset_paused`/`enter_leap_idle_pause` returns it to Paused.
    #[cfg(test)]
    pub(crate) fn set_phase_sampling(&mut self) {
        self.phase = DutyPhase::Sampling;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    #[test]
    fn sample_window_shorter_than_period() {
        assert!(IDLE_PAUSE_SAMPLE_WINDOW < IDLE_PAUSE_PERIOD);
    }

    #[test]
    fn reset_paused_pauses_immediately() {
        let mut d = LeapIdlePause::default();
        assert_eq!(d.reset_paused(at(1000)), PauseAction::Pause);
        assert_eq!(d.phase(), DutyPhase::Paused);
    }

    #[test]
    fn holds_during_gap_then_resumes() {
        let mut d = LeapIdlePause::default(); // gap = 350 ms
        d.reset_paused(Duration::ZERO);
        assert_eq!(d.advance(at(100)), PauseAction::Hold);
        assert_eq!(d.advance(at(349)), PauseAction::Hold);
        assert_eq!(d.advance(at(350)), PauseAction::Resume);
        assert_eq!(d.phase(), DutyPhase::Sampling);
    }

    #[test]
    fn holds_during_window_then_pauses() {
        let mut d = LeapIdlePause::default(); // window = 150 ms
        d.reset_paused(Duration::ZERO);
        d.advance(at(350)); // -> Sampling, phase_since = 350
        assert_eq!(d.advance(at(400)), PauseAction::Hold);
        assert_eq!(d.advance(at(499)), PauseAction::Hold);
        assert_eq!(d.advance(at(500)), PauseAction::Pause);
        assert_eq!(d.phase(), DutyPhase::Paused);
    }

    #[test]
    fn requested_wake_short_while_sampling() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO);
        d.advance(at(350)); // -> Sampling
        assert_eq!(d.requested_wake(at(360)), SAMPLE_POLL_INTERVAL);
    }

    #[test]
    fn requested_wake_counts_down_during_gap() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO); // Paused, gap 350
        assert_eq!(d.requested_wake(at(0)), at(350));
        assert_eq!(d.requested_wake(at(100)), at(250));
    }

    #[test]
    fn full_cycle_repeats() {
        let mut d = LeapIdlePause::default();
        d.reset_paused(Duration::ZERO);
        assert_eq!(d.advance(at(350)), PauseAction::Resume);
        assert_eq!(d.advance(at(500)), PauseAction::Pause);
        assert_eq!(d.advance(at(850)), PauseAction::Resume);
        assert_eq!(d.advance(at(1000)), PauseAction::Pause);
    }
}
```

- [ ] **Step 3: Run the tests; verify they pass.**

Run: `cargo nextest run -p wc-core --all-features idle_pause`
Expected: all 7 tests PASS.

- [ ] **Step 4: Run the gates, then commit.**

```bash
git add crates/wc-core/src/input/idle_pause.rs crates/wc-core/src/input/mod.rs
git commit -m "input/idle_pause: duty-cycle state machine for deep-idle Leap throttle"
```

---

## Phase 4 — Wire to Bevy + the live provider + present-rate reconciliation

### Task 5: Duty-cycle systems on the Leap provider

**Files:**
- Modify: `crates/wc-core/src/input/providers/leap_native.rs:668-704` (the "screensaver idle-pause" section + its tests)
- Modify: `crates/wc-core/src/input/mod.rs:122-133` (registration)

- [ ] **Step 1: Replace the dead-code section with the duty-cycle systems.** In `crates/wc-core/src/input/providers/leap_native.rs`, replace `pause_leap_on_screensaver` (`:686-697`) — it's superseded — and keep `resume_leap_on_active`. The `set_all_leap_paused` helper (`:668-684`) stays. Add imports near the top of the file's `use` block:

```rust
use crate::input::idle_pause::{LeapIdlePause, PauseAction};
```

Then make the section read:

```rust
/// Apply a [`PauseAction`] from the duty cycle to every registered Leap provider.
fn apply_pause_action(action: PauseAction, registry: &mut crate::input::provider::ProviderRegistry) {
    match action {
        PauseAction::Pause => set_all_leap_paused(registry, true),
        PauseAction::Resume => set_all_leap_paused(registry, false),
        PauseAction::Hold => {}
    }
}

/// `OnEnter(SketchActivity::Screensaver)` — begin the idle-pause duty cycle: pause
/// the Leap service immediately (no visitor at deep-idle entry) and arm the
/// sampling clock (Plan `leap-idle-pause`, CF #84).
pub fn enter_leap_idle_pause(
    time: Res<'_, bevy::time::Time>,
    mut duty: ResMut<'_, LeapIdlePause>,
    mut registry: ResMut<'_, crate::input::provider::ProviderRegistry>,
) {
    let action = duty.reset_paused(time.elapsed());
    apply_pause_action(action, &mut registry);
}

/// `Update` while the screensaver is showing — advance the duty cycle, toggling
/// the Leap service pause on phase boundaries. Brief sample windows let a hand
/// wake the install (its frames flow through `reset_on_interaction` → `Active`,
/// where [`resume_leap_on_active`] un-pauses for good).
pub fn drive_leap_idle_pause(
    time: Res<'_, bevy::time::Time>,
    mut duty: ResMut<'_, LeapIdlePause>,
    mut registry: ResMut<'_, crate::input::provider::ProviderRegistry>,
) {
    let action = duty.advance(time.elapsed());
    apply_pause_action(action, &mut registry);
}
```

Keep `resume_leap_on_active` exactly as-is (`:699-704`). Update the section banner comment (`:668`) to "screensaver idle-pause duty cycle (leap-idle-pause, CF #84)".

- [ ] **Step 2: Add a wiring test** to the `#[cfg(test)] mod tests` block at the bottom of `leap_native.rs` (it compiles under `--all-features`; runs without hardware because an empty registry makes `apply_pause_action` a no-op):

```rust
    #[test]
    fn enter_resets_duty_cycle_to_paused() {
        use bevy::ecs::system::RunSystemOnce;
        use bevy::prelude::*;
        use crate::input::idle_pause::{DutyPhase, LeapIdlePause};
        use crate::input::provider::ProviderRegistry;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins); // provides Time
        app.init_resource::<LeapIdlePause>();
        app.init_resource::<ProviderRegistry>();

        // Pretend we were mid-sample, then enter the screensaver.
        app.world_mut().resource_mut::<LeapIdlePause>().set_phase_sampling();
        app.world_mut()
            .run_system_once(super::enter_leap_idle_pause)
            .expect("system run");

        assert_eq!(
            app.world().resource::<LeapIdlePause>().phase(),
            DutyPhase::Paused,
            "entering the screensaver must pause + arm the duty cycle",
        );
    }
```

- [ ] **Step 3: Register the systems.** In `crates/wc-core/src/input/mod.rs`, extend the existing `#[cfg(feature = "hand-tracking-gestures")]` block (`:126-132`) so it also inits the resource and wires the three systems. Add the imports it needs at the top of the existing block / file:

```rust
        #[cfg(feature = "hand-tracking-gestures")]
        {
            use crate::lifecycle::screensaver::ScreensaverActive;
            use crate::lifecycle::state::SketchActivity;

            app.init_resource::<self::idle_pause::LeapIdlePause>();

            // Begin the duty cycle on deep-idle entry; un-pause for good when a
            // visitor returns. (`apply_leap_background_setting` stays as below.)
            app.add_systems(
                OnEnter(SketchActivity::Screensaver),
                self::providers::leap_native::enter_leap_idle_pause,
            );
            app.add_systems(
                OnEnter(SketchActivity::Active),
                self::providers::leap_native::resume_leap_on_active,
            );
            app.add_systems(
                Update,
                self::providers::leap_native::drive_leap_idle_pause
                    .run_if(resource_exists::<ScreensaverActive>),
            );

            app.add_systems(
                PreUpdate,
                self::providers::leap_native::apply_leap_background_setting
                    .after(systems::poll_all_providers)
                    .in_set(InputSystems),
            );
        }
```

(Delete the old standalone `apply_leap_background_setting` registration at `:127-132` — it's folded into the block above so there's a single feature-gated block.)

- [ ] **Step 4: Run the wiring test + state-machine tests.**

Run: `cargo nextest run -p wc-core --all-features 'enter_resets_duty_cycle_to_paused' && cargo nextest run -p wc-core --all-features idle_pause`
Expected: PASS. (`RunSystemOnce` is in `bevy::ecs::system`; if the import path differs in this Bevy build, adjust per `cargo doc`.)

- [ ] **Step 5: Confirm no remaining references to the removed system.**

Run: `rg -n "pause_leap_on_screensaver" crates/ docs/`
Expected: only doc/ledger mentions (which Task 7 updates) — no `crates/` references. Fix any stragglers.

- [ ] **Step 6: Run the gates, then commit.**

```bash
git add crates/wc-core/src/input/providers/leap_native.rs crates/wc-core/src/input/mod.rs
git commit -m "input/leap: duty-cycled idle-pause systems wired to screensaver lifecycle (CF #84)"
```

### Task 6: Present-rate reconciliation

**Files:**
- Modify: `crates/wc-core/src/lifecycle/screensaver/mod.rs:184-212` + its tests

- [ ] **Step 1: Write the failing test.** Add to the `#[cfg(test)] mod tests` block in `screensaver/mod.rs`:

```rust
    #[test]
    fn effective_wait_is_floored_to_duty_cycle() {
        // Hot tier present wait (333 ms) yields to a tighter duty-cycle wake.
        let tier = Duration::from_millis(333);
        assert_eq!(effective_wait(tier, Some(Duration::from_millis(16))), Duration::from_millis(16));
        // No duty cycle → tier wait unchanged.
        assert_eq!(effective_wait(tier, None), tier);
        // Duty cycle slower than tier (long gap) → tier wait wins.
        assert_eq!(effective_wait(tier, Some(Duration::from_millis(350))), tier);
    }
```

- [ ] **Step 2: Run it; verify it fails.**

Run: `cargo nextest run -p wc-core --all-features effective_wait_is_floored_to_duty_cycle`
Expected: FAIL — `effective_wait` not defined.

- [ ] **Step 3: Add the pure helper + use it in `apply_present_rate`.** In `screensaver/mod.rs`, add the helper near `tier_present_wait`:

```rust
/// The reactive present `wait`: the tier's wait, floored to the Leap duty cycle's
/// requested wake so the gap ends on time and sample windows are polled fast
/// enough to catch a resume frame. `None` when the duty cycle is absent (the
/// `hand-tracking-gestures` feature is off, or no Leap is installed).
#[must_use]
fn effective_wait(tier_wait: Duration, duty_wake: Option<Duration>) -> Duration {
    match duty_wake {
        Some(w) => tier_wait.min(w),
        None => tier_wait,
    }
}
```

Then extend `apply_present_rate`'s signature and body to consult it. Add params:

```rust
fn apply_present_rate(
    thermal: Res<'_, ThermalState>,
    time: Res<'_, Time>,
    duty: Option<Res<'_, crate::input::idle_pause::LeapIdlePause>>,
    #[cfg(debug_assertions)] toggles: Option<Res<'_, DebugToggles>>,
    #[cfg(debug_assertions)] capture: Option<Res<'_, crate::capture::config::CaptureConfig>>,
    mut winit: ResMut<'_, WinitSettings>,
) {
```

and replace the `let wait = tier_present_wait(tier);` line (`:201`) with:

```rust
    let duty_wake = duty.as_deref().map(|d| d.requested_wake(time.elapsed()));
    let wait = effective_wait(tier_present_wait(tier), duty_wake);
```

(`LeapIdlePause` is always compiled — `idle_pause` is not feature-gated — so no `cfg` is needed here; the resource simply doesn't exist without the feature, giving `None`.)

- [ ] **Step 4: Run the test; verify it passes.**

Run: `cargo nextest run -p wc-core --all-features effective_wait_is_floored_to_duty_cycle`
Expected: PASS.

- [ ] **Step 5: Run the gates, then commit.**

```bash
git add crates/wc-core/src/lifecycle/screensaver/mod.rs
git commit -m "lifecycle/screensaver: floor idle present-rate to the Leap duty-cycle wake"
```

---

## Phase 5 — On-hardware tuning + sign-off (conditional on Phase 1 go)

> Operator (Madison) + hardware.

### Task 7: Tune the window, verify wake, update the ledger

**Files:**
- Modify: `crates/wc-core/src/input/idle_pause.rs` (`IDLE_PAUSE_SAMPLE_WINDOW`)
- Modify: `docs/superpowers/roadmap.md`, `docs/superpowers/next-plan-carry-forwards.md`

- [ ] **Step 1: Set the sample window from the measured latency.** Using the Phase 1 median `L`, set `IDLE_PAUSE_SAMPLE_WINDOW` to `L` + ~50 ms margin (e.g., median 120 ms → `Duration::from_millis(170)`). Keep it well below `IDLE_PAUSE_PERIOD` (the `sample_window_shorter_than_period` test guards this).

- [ ] **Step 2: Run the state-machine tests** (the `holds_during_window_then_pauses` / `full_cycle_repeats` boundaries assume 150 ms; update those test constants to match the new window if you changed it).

Run: `cargo nextest run -p wc-core --all-features idle_pause`
Expected: PASS.

- [ ] **Step 3: OPERATOR — live wake test.** Launch the app (`cargo rund`), let it idle into the screensaver, then present a hand. Confirm: (a) it wakes within ~0.5 s without having to hold/wave longer than that; (b) `top`/Activity Monitor shows the Ultraleap daemon CPU dropping during the paused gaps; (c) mouse still wakes instantly. Optionally IR-viewer check whether the controller LEDs dim while paused (host-CPU saving is the primary win regardless — see `set_paused`'s honest caveat at `leap_native.rs:317`).

- [ ] **Step 4: Update the ledger.** In `docs/superpowers/next-plan-carry-forwards.md`, mark **CF #84** RESOLVED (cite this plan + the measured L and chosen window). In `docs/superpowers/roadmap.md`, under `leap-idle-pause` note it shipped as a duty cycle (not a flat pause), record the prerequisite `reset_on_interaction` fix, and (if Phase 1 said go) add a Shipped-history row; tag `v5-leap-idle-pause`.

- [ ] **Step 5: Run the full gate suite, then commit + tag.**

```bash
git add crates/wc-core/src/input/idle_pause.rs docs/superpowers/roadmap.md docs/superpowers/next-plan-carry-forwards.md
git commit -m "input/idle_pause: tune sample window to measured resume latency; close CF #84"
git tag v5-leap-idle-pause
```

---

## Self-review (completed against the design)

**Spec coverage:**
- CF #84 (unwired idle-pause) → Tasks 5 (wiring), 7 (sign-off/tag). ✔
- Madison's duty-cycle design + 0.5 s target → Task 4 (`IDLE_PAUSE_PERIOD = 500 ms`), Task 7 (window tuned to L). ✔
- Keep hand-wake alive → duty cycle (Task 4/5) + the prerequisite interaction fix (Task 3) + `resume_leap_on_active` retained (Task 5). ✔
- Present-rate ↔ duty-cycle interaction → Task 6. ✔
- Hardware unknown (L) gated before building → Phase 1 spike + decision gate. ✔
- Testability wrinkle (no `LeaprsProvider` without hardware) → pure state machine tested in CI (Task 4), wiring tested via empty-registry no-op (Task 5), pause behaviour verified on hardware (Tasks 2, 7). ✔

**Placeholder scan:** `IDLE_PAUSE_SAMPLE_WINDOW = 150 ms` is explicitly a tuned-in-Phase-5 placeholder with a guarding invariant test — not an unfilled blank. No "TBD"/"add error handling"/"similar to" placeholders remain.

**Type consistency:** `LeapIdlePause` / `DutyPhase` / `PauseAction` / `reset_paused` / `advance` / `requested_wake` / `phase` / `set_phase_sampling` / `effective_wait` / `apply_pause_action` / `enter_leap_idle_pause` / `drive_leap_idle_pause` / `resume_leap_on_active` are used identically across Tasks 4–6. `requested_wake(now)` takes `Time::elapsed()` everywhere it's called.

**Open risks to watch during execution:**
- If Phase 1 returns L ≥ ~0.4 s, **stop after Phase 2** and take the fallback branch (don't build Phases 3–5); Phase 2 still ships.
- `bevy::ecs::system::RunSystemOnce` and `world.write_message` are the Bevy-0.18 spellings; confirm against `cargo doc` if a call doesn't resolve.
- `HandTrackingFrame`'s `Default` / public fields (Task 3) and `LeaprsProvider` public reachability (Task 2 Step 3) are confirmed-or-fix-forward in their steps.
