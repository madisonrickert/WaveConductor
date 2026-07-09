# Audio Output Device Selection and Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sound comes out of the operator's chosen output (an HDMI TV), and keeps coming out of it for an eight-hour soak — surviving a sleeping TV, an input switch, or any single endpoint blip without a human restart.

**Architecture:** Two halves. **(1) Recovery (higher value, lands first, no UI):** a main-thread *supervisor* replaces the terminal `AudioStatus::Errored`. The cpal error-callback flag (already present) drives `AudioStatus::Reconnecting`; the supervisor rebuilds the cpal stream on an exponential backoff (1 s, 2 s, 4 s, 8 s, 16 s, capped 30 s), re-resolving the device, recreating the rings, and restoring play/pause from `AppState`. A background *device-watcher* OS thread polls output-device topology every ~2 s (WASAPI enumeration can block, so it is off both the audio callback and the render thread) and hands the main thread a fresh name list, which also triggers an early rebuild when the saved endpoint reappears. **(2) Selection (blocked on Plan 03a's widget):** a new `AudioSettings` section persists the chosen device *by name*; startup and every rebuild resolve that name against the live list with a fallback to the system default; the settings panel renders the choice with Plan 03a's runtime-enumerated dropdown fed by an `AvailableAudioDevices` resource.

**Tech Stack:** Rust, Bevy 0.19, `cpal` 0.16, `rtrb` lock-free rings, `std::thread` + `std::sync::mpsc` for the watcher, `#[derive(SketchSettings)]` for persistence.

**Depends on:** Plan 03a (runtime-enumerated setting widget) — for the UI tasks (6 and 7) only. The recovery tasks (1–5) depend on nothing and land first.

## Global Constraints

Copied from `AGENTS.md` and the program index's Part 1 (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`). Every task's requirements implicitly include this section.

- **Audio-thread real-time contract is unchanged.** The cpal data callback and error callback are real-time threads: **lock-free ring buffers only, no `Mutex`, no allocation, no logging, no blocking after init.** The error callback still does exactly one thing — a single relaxed atomic store into `AudioErrorFlag`. Nothing this plan adds runs on the audio callback.
- **Never allocate in a hot path**, where a hot path is *any* code that runs repeatedly for the life of a session: per-frame Bevy systems, egui paint-callback hooks, the audio callback, **and continuously-running worker/background threads**. The device-watcher thread counts. Pre-allocate and reuse (`vec.clear()` keeps capacity); where a dependency's API forces a residual allocation, document the exact cost inline and flag it as a profiling-gated follow-up rather than leaving it silent.
- **No new dependencies.** `cpal`, `rtrb`, `bevy`, `serde`, `thiserror`, `smallvec` are already in the graph. `std::thread` and `std::sync::mpsc` are std. Confirm with `cargo tree -i <crate>` before reaching for anything new; do not add one.
- **No `unwrap()` / `expect()` in non-test code** unless the panic is a documented invariant violation. Use `let … else`, `match`, `?`, `Option`/`Result` combinators.
- **No `as` casts on numeric types** where `From` / `TryFrom` / `u32::try_from` / `u64::try_from` would work.
- `///` rustdoc on **every** public item (struct, enum, trait, fn, const, module). `//!` module header on every new module root describing role and which thread each piece runs on.
- **Never strip comments during refactors.** Update stale comments in place.
- Public API at the top of a file, private helpers below, `#[cfg(test)] mod tests` at the footer. One concept per file (~300 lines is a guideline).
- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings`
  - `cargo nextest run --workspace --all-features` (nextest skips doctests)
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs it with `RUSTDOCFLAGS="-D warnings"`; **no `--all-features`**)
  - `cargo deny check`
  - `cargo xtask check-secrets`
- **The per-task clippy gate uses `--all-targets`**, never `--lib`. `--lib` skips the test target; in Plan 01 that gap hid `range_plus_one` and `used_underscore_binding` in the plan's own test code.
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** In example/test code: no `.unwrap()`/`.expect()`/`panic!` (a `#[cfg(test)] mod tests` block gets `#[allow(clippy::expect_used, …, reason = "…")]` if it truly needs them, exactly as `state.rs` and `hand_tracking.rs` already do); no `assert_eq!(x.is_some(), true)` (use `assert!(x.is_some())`); no `0..(N + 1)` (use `0..=N`); no leading-underscore bindings you then read.
- **A type introduced before its first non-test caller is dead code** and fails `-D warnings` on the lib target. Where this plan lands a type in one task and its caller in a later one, the introducing task adds a transient `#![allow(dead_code)]` **with an explicit deletion step scheduled in the task that adds the first real caller** (the Plan 01 pattern). Do not leave the allow in place.
- **The doc gate denies public→private intra-doc links** (`rustdoc::private_intra_doc_links`). A `pub` item's rustdoc must not `[link]` to a `pub(crate)`/private item — demote to a plain code span.
- **Commit with `git commit -F <file>`, never `-m`.** Backticks in a `-m` string are command-substituted by zsh and silently eat words. Write the message to a file (a `<<'EOF'` heredoc with the **quoted** delimiter prevents substitution) and `git commit -F`.
- **Stage named paths only. Never `git add -A`.** After committing, `git show --stat HEAD` to confirm exactly the intended files landed.
- **There are no audio-device or GPU tests in CI.** CI has no output device. Every behavioural guarantee in this plan is a **pure unit test** (backoff schedule, device-name resolution, topology diff, supervisor state machine) that passes headlessly. Anything requiring a real endpoint is an explicit **human** `cargo rund` step — an agent cannot verify it.
- **`cargo rund`** is the manual-smoke command (fast dynamic-linked debug). Never launch the bare `target/` binary.

---

## Task ordering and the 03a boundary

```
Recovery half (no UI, depends on nothing):
  1  AudioStatus::Reconnecting            (state.rs; drop the terminal message)
  2  supervisor.rs pure logic             (backoff + AudioSupervisor state machine)
  3  device.rs pure logic                 (name resolver + topology diff + resources)
  4  device-watcher thread + drain system (native; wired into AudioPlugin)
  5  engine rebuild + supervise_audio      (ties 1–4 together; removes the transient allows)

Selection / UI half (BLOCKED ON PLAN 03a):
  6  AudioSettings { output_device }        (persist by name; wire into the resolver)
  7  panel row consuming 03a's widget       (dropdown fed by AvailableAudioDevices)
```

Tasks 1–5 are strictly ordered and self-contained; after Task 5 the installation recovers audio on its own with **zero UI**. Tasks 6–7 must not begin until Plan 03a has merged its runtime-enumerated widget.

---

### Task 1: Add `AudioStatus::Reconnecting`; make the stream death recoverable, not terminal

**Files:**
- Modify: `crates/wc-core/src/audio/state.rs` (enum at `:38-48`; the callback block at `:179-187`; the helper at `:190-203`; the tests at `:205-243`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `AudioStatus::Reconnecting` variant.
  - `pub(super) fn mark_reconnecting_from_callback(state: &mut AudioState) -> bool` (replaces `set_errored_from_callback`; returns `true` only on the transition *into* `Reconnecting`, so the caller logs once).

**Why.** `state.rs:185` logs *"Status set to Errored. Restart the app to recover audio."* and means it — one HDMI blip silences the install for the night. A mid-run stream death must instead enter a **recoverable** `Reconnecting` state that the Task 5 supervisor drives. The hard-failure `Errored` variant stays for the genuinely unrecoverable startup case (`EngineBuildError` with no device at all) and for the explicit `AudioMessage::Errored` path.

- [ ] **Step 1: Find every match on `AudioStatus`**

The new variant can break an exhaustive `match`. Run:

```bash
rg -n "AudioStatus::" crates/ --glob '!*/audio/state.rs'
```

Expected today: only `crates/wc-core/src/audio/engine.rs:109` (`= AudioStatus::Errored;`, an assignment — safe) and `crates/wc-core/tests/audio.rs` (assertions — safe). If any `match state.status { … }` without a wildcard arm appears (e.g. a future status label), it must gain a `Reconnecting` arm in this task. Record what you found in the commit body.

- [ ] **Step 2: Write the failing tests**

Replace the two flag/callback tests in `crates/wc-core/src/audio/state.rs:220-242` (`error_callback_transitions_running_to_errored_once` and `error_flag_swap_consumes_the_flag`) with:

```rust
    #[test]
    fn callback_transitions_running_to_reconnecting_once() {
        let mut state = AudioState {
            status: AudioStatus::Running,
            ..AudioState::default()
        };
        // First observation transitions and reports `true` (so the caller logs).
        assert!(mark_reconnecting_from_callback(&mut state));
        assert_eq!(state.status, AudioStatus::Reconnecting);
        assert_eq!(state.last_error.as_deref(), Some(ERROR_CALLBACK_MESSAGE));
        // A second observation is idempotent and reports `false` (no re-log).
        assert!(!mark_reconnecting_from_callback(&mut state));
        assert_eq!(state.status, AudioStatus::Reconnecting);
    }

    #[test]
    fn error_flag_swap_consumes_the_flag() {
        let flag = AudioErrorFlag(Arc::new(AtomicBool::new(true)));
        // The pump consumes the flag with `swap`; the first read sees `true`,
        // subsequent reads see `false` until the callback sets it again.
        assert!(flag.0.swap(false, Ordering::Relaxed));
        assert!(!flag.0.swap(false, Ordering::Relaxed));
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib audio::state 2>&1 | head -20`

Expected: FAIL to compile — `cannot find value AudioStatus::Reconnecting in this scope` and `cannot find function mark_reconnecting_from_callback in this scope`.

- [ ] **Step 4: Add the variant**

In `crates/wc-core/src/audio/state.rs`, extend the `AudioStatus` enum (currently `:38-48`) — insert the new variant before `Errored`:

```rust
/// Lifecycle status of the audio engine, mirrored from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Reflect, Default)]
pub enum AudioStatus {
    /// The Startup system has not yet run, or failed to build the stream.
    #[default]
    NotStarted,
    /// The audio thread is running and rendering samples.
    Running,
    /// The stream died mid-run (a device blip: TV asleep, input switch,
    /// endpoint removed) and the supervisor is rebuilding it on a backoff.
    /// This is a *recoverable* state; `AudioStatus::Errored` is not. See
    /// `supervisor::supervise_audio`.
    Reconnecting,
    /// The audio thread failed unrecoverably: no output device exists at all,
    /// or an explicit `AudioMessage::Errored`. See `last_error` in
    /// [`AudioState`].
    Errored,
}
```

(The doc comment on `Reconnecting` references `supervise_audio` as a plain code span, not an intra-doc link: that item does not exist until Task 5 and is `pub(crate)`, so a link would break the doc gate.)

- [ ] **Step 5: Rewrite the callback handling in `pump_audio_messages`**

Replace the flag block at `crates/wc-core/src/audio/state.rs:175-187` (from the `// Surface a mid-run stream death.` comment through the closing brace of the `if`):

```rust
    // Surface a mid-run stream death. The error callback stores `true` and
    // never logs (real-time thread); `swap` consumes the flag so we act at most
    // once per error event, and `mark_reconnecting_from_callback` reports
    // whether this was the transition into `Reconnecting` so we log exactly
    // once. The supervisor (`supervisor::supervise_audio`) owns the rebuild
    // from here; this pump only flips the status so the supervisor picks it up.
    let callback_fired = error_flag
        .as_ref()
        .is_some_and(|flag| flag.0.swap(false, Ordering::Relaxed));
    if callback_fired && mark_reconnecting_from_callback(&mut state) {
        tracing::warn!(
            "cpal stream error callback fired; audio stream died. \
             Entering Reconnecting — the supervisor will rebuild it."
        );
    }
```

- [ ] **Step 6: Rewrite the helper**

Replace `set_errored_from_callback` (`crates/wc-core/src/audio/state.rs:190-203`) with:

```rust
/// Drive [`AudioState`] into [`AudioStatus::Reconnecting`] in response to the
/// cpal error callback firing (a recoverable mid-run stream death).
///
/// Returns `true` only when this call *transitioned* the status into
/// `Reconnecting`, so the caller logs exactly once per failure rather than
/// every `PreUpdate` while the stream is down. Sets [`AudioState::last_error`]
/// to [`ERROR_CALLBACK_MESSAGE`] (the callback cannot format the underlying
/// error without allocating on its real-time thread).
///
/// A stream that is already `Reconnecting` (or has since gone `Errored` on a
/// hard failure) is left as-is and reports `false`.
pub(super) fn mark_reconnecting_from_callback(state: &mut AudioState) -> bool {
    let newly = state.status != AudioStatus::Reconnecting && state.status != AudioStatus::Errored;
    if newly {
        state.status = AudioStatus::Reconnecting;
    }
    state.last_error = Some(ERROR_CALLBACK_MESSAGE.to_string());
    newly
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib audio::state`

Expected: PASS (the two rewritten tests plus the untouched `default_state_is_not_started_unmuted_full_volume`).

- [ ] **Step 8: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib audio::state
git add crates/wc-core/src/audio/state.rs
git commit -F - <<'EOF'
feat(audio/state): add Reconnecting status; stream death is recoverable

state.rs logged "Status set to Errored. Restart the app to recover audio"
on any mid-run cpal error and meant it -- one HDMI blip (a TV sleeping, an
input switch) silenced an unattended installation for the night.

Add AudioStatus::Reconnecting and repoint the error-callback path at it:
mark_reconnecting_from_callback replaces set_errored_from_callback and
transitions Running -> Reconnecting once, logging at warn. The terminal
Errored variant stays for the genuinely unrecoverable startup case (no
output device at all) and the explicit AudioMessage::Errored path. The
Task 5 supervisor drives the rebuild from Reconnecting.
EOF
git show --stat HEAD
```

---

### Task 2: `supervisor.rs` — the pure reconnect state machine (backoff, no I/O)

**Files:**
- Create: `crates/wc-core/src/audio/supervisor.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (add `pub mod supervisor;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const BACKOFF_CAP: Duration`
  - `pub fn backoff_delay(attempt: u32) -> Duration`
  - `pub enum SupervisorAction { Idle, Rebuild }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct AudioSupervisor` (`Resource, Debug, Default, Clone`) with `begin`, `request_now`, `poll`, `record_failure`, `record_success`, and `#[cfg(test)]` accessors `attempts`, `is_reconnecting`.

**Why pure.** Everything here is arithmetic over a monotonic clock supplied as `f64` seconds. No cpal, no threads, no Bevy `Time` — so the full state machine is unit-tested headlessly. Task 5's Bevy system is the only thing that reads the wall clock and calls the rebuild; it delegates every *decision* to this module.

- [ ] **Step 1: Register the module and write the failing tests**

In `crates/wc-core/src/audio/mod.rs`, next to the other `pub mod` lines, add `pub mod supervisor;`. Then create `crates/wc-core/src/audio/supervisor.rs` containing **only** the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_thirty_seconds() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        // 2^5 = 32 > cap.
        assert_eq!(backoff_delay(5), BACKOFF_CAP);
        assert_eq!(backoff_delay(6), BACKOFF_CAP);
        // Absurd attempt counts saturate rather than overflow the shift.
        assert_eq!(backoff_delay(1_000), BACKOFF_CAP);
    }

    #[test]
    fn begin_schedules_the_first_attempt_one_second_out() {
        let mut sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        sup.begin(100.0);
        assert!(sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        // Nothing due before the 1 s backoff elapses.
        assert_eq!(sup.poll(100.5), SupervisorAction::Idle);
        // Due exactly at the deadline.
        assert_eq!(sup.poll(101.0), SupervisorAction::Rebuild);
    }

    #[test]
    fn repeated_failures_grow_the_backoff() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        assert_eq!(sup.poll(1.0), SupervisorAction::Rebuild);
        sup.record_failure(1.0); // attempt 1 -> next at 1.0 + 2 s
        assert_eq!(sup.attempts(), 1);
        assert_eq!(sup.poll(2.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(3.0), SupervisorAction::Rebuild);
        sup.record_failure(3.0); // attempt 2 -> next at 3.0 + 4 s
        assert_eq!(sup.poll(6.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(7.0), SupervisorAction::Rebuild);
    }

    #[test]
    fn success_clears_the_reconnect_cycle() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        sup.record_failure(1.0);
        sup.record_success();
        assert!(!sup.is_reconnecting());
        assert_eq!(sup.attempts(), 0);
        // Nothing is ever due once cleared.
        assert_eq!(sup.poll(1_000.0), SupervisorAction::Idle);
    }

    #[test]
    fn request_now_forces_an_immediate_attempt_without_resetting_attempts() {
        let mut sup = AudioSupervisor::default();
        sup.begin(0.0);
        sup.record_failure(0.0); // attempt 1, next at 2 s
        assert_eq!(sup.poll(0.5), SupervisorAction::Idle);
        // A device reappearing short-circuits the wait.
        sup.request_now(0.5);
        assert_eq!(sup.poll(0.5), SupervisorAction::Rebuild);
        // The failure count is preserved so backoff keeps growing if it fails again.
        assert_eq!(sup.attempts(), 1);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib audio::supervisor 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function backoff_delay`, `cannot find type AudioSupervisor`, etc.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/wc-core/src/audio/supervisor.rs`, above the test module:

```rust
//! Reconnect supervision for the audio engine.
//!
//! ## Why this exists
//!
//! A single output-endpoint blip — an HDMI TV going to sleep, an input switch,
//! a device re-enumeration — used to leave the app in a terminal
//! [`crate::audio::state::AudioStatus::Errored`] with a "restart the app" log.
//! This module replaces that dead end with a bounded exponential-backoff
//! rebuild loop.
//!
//! ## What runs where
//!
//! **This file is pure.** Every function is arithmetic over a monotonic clock
//! passed in as `f64` seconds; there is no cpal call, no thread, and no Bevy
//! `Time` here, which is what makes the whole state machine unit-testable with
//! no audio device (CI has none). Task 5's `supervise_audio` Bevy system is the
//! only place the wall clock is read and the actual stream rebuild is performed
//! — and it lives on the **main thread**, never the audio callback and never
//! the render thread. The rebuild itself (which can block on WASAPI
//! enumeration) is event-driven (error path / backoff tick), not a per-frame
//! cost.

use std::time::Duration;

use bevy::prelude::Resource;

/// Ceiling on the reconnect backoff. The delay doubles from 1 s and is clamped
/// here so an install left with a permanently-absent device retries at a steady
/// once-every-30-s rather than drifting toward hours.
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Backoff before the `attempt`-th reconnect: `2^attempt` seconds (1, 2, 4, 8,
/// 16, …) clamped to [`BACKOFF_CAP`]. Attempt 0 is the first retry after a
/// failure, at 1 s.
///
/// The shift saturates: an implausibly large `attempt` yields the cap rather
/// than overflowing.
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    // `checked_shl` returns None past 63 bits; treat that as "very large".
    let secs = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(BACKOFF_CAP)
}

/// What [`AudioSupervisor::poll`] tells the caller to do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorAction {
    /// No attempt is due; do nothing this tick.
    Idle,
    /// A (re)build attempt is due now.
    Rebuild,
}

/// Reconnect bookkeeping: the consecutive-failure counter and the scheduled
/// time of the next attempt, both driven by the caller's monotonic clock.
///
/// A Bevy `Resource` so the main-thread supervisor system owns exactly one.
/// Times are `f64` seconds on whatever monotonic clock the caller uses
/// (Task 5 uses `Time<Real>::elapsed_secs_f64`); this type never reads a clock
/// itself.
#[derive(Resource, Debug, Default, Clone)]
pub struct AudioSupervisor {
    /// Consecutive failed (re)build attempts. Grows the backoff; reset on
    /// success.
    attempts: u32,
    /// Monotonic time of the next scheduled attempt, or `None` when no
    /// reconnect cycle is in progress.
    next_attempt_at: Option<f64>,
}

impl AudioSupervisor {
    /// Start (or restart) a reconnect cycle: reset the failure count and
    /// schedule the first attempt one backoff step out (`now + 1 s`).
    ///
    /// Idempotent while a cycle is already running is *not* assumed — call this
    /// only on the edge into `Reconnecting`.
    pub fn begin(&mut self, now: f64) {
        self.attempts = 0;
        self.next_attempt_at = Some(now + backoff_delay(0).as_secs_f64());
    }

    /// Bring the next attempt forward to `now` without touching the failure
    /// count. Used when the saved endpoint reappears in the device list, so the
    /// stream migrates back immediately instead of waiting out the backoff.
    pub fn request_now(&mut self, now: f64) {
        self.next_attempt_at = Some(now);
    }

    /// Whether a (re)build attempt is due at `now`.
    #[must_use]
    pub fn poll(&self, now: f64) -> SupervisorAction {
        match self.next_attempt_at {
            Some(at) if now >= at => SupervisorAction::Rebuild,
            _ => SupervisorAction::Idle,
        }
    }

    /// Record a failed attempt: grow the backoff and reschedule.
    pub fn record_failure(&mut self, now: f64) {
        self.attempts = self.attempts.saturating_add(1);
        self.next_attempt_at = Some(now + backoff_delay(self.attempts).as_secs_f64());
    }

    /// Record a successful (re)build: clear the cycle so nothing is due until
    /// the next stream death calls [`Self::begin`].
    pub fn record_success(&mut self) {
        self.attempts = 0;
        self.next_attempt_at = None;
    }

    /// Consecutive-failure count. Test-only: production reads it only through
    /// [`Self::poll`]'s scheduling. Gated so the lib target does not flag it as
    /// dead code under `-D warnings`.
    #[cfg(test)]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Whether a reconnect cycle is in progress. Test-only; gated as above.
    #[cfg(test)]
    pub fn is_reconnecting(&self) -> bool {
        self.next_attempt_at.is_some()
    }
}
```

> **Transient dead-code allow.** `AudioSupervisor`, `SupervisorAction`, `backoff_delay`, and `BACKOFF_CAP` have no non-test caller until Task 5 wires `supervise_audio`. Add this at the very top of the file (above the `//!` header is not valid; place it immediately **after** the `//!` header block and before `use`):

```rust
// Transient. Nothing outside this module's tests calls into the supervisor
// until Task 5 adds `supervise_audio`. Until then the lib target (compiled
// without cfg(test)) sees these items as dead code and `clippy -D warnings`
// fails. Task 5 removes this attribute and verifies clippy stays clean.
#![allow(dead_code)]
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib audio::supervisor`

Expected: PASS, 5 tests.

- [ ] **Step 5: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib audio::supervisor
git add crates/wc-core/src/audio/supervisor.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio/supervisor): pure reconnect state machine (exponential backoff)

Backoff doubles from 1 s and caps at 30 s; AudioSupervisor tracks the
failure count and the next-attempt deadline against a monotonic clock the
caller supplies as f64 seconds. No cpal, no threads, no Bevy Time here, so
the whole state machine is unit-tested headlessly (CI has no audio device).
Task 5's main-thread system reads the wall clock and performs the rebuild;
it delegates every decision to this module.

Carries a transient #![allow(dead_code)] removed in Task 5 when
supervise_audio becomes its first non-test caller.
EOF
git show --stat HEAD
```

---

### Task 3: `device.rs` — device-name resolution, topology diff, and the shared resources

**Files:**
- Create: `crates/wc-core/src/audio/device.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (add `pub mod device;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum DeviceResolution { Preferred(String), Fallback { saved_unavailable: Option<String> } }` (`Debug, Clone, PartialEq, Eq`)
  - `pub fn resolve_output_device(saved: Option<&str>, available: &[String]) -> DeviceResolution`
  - `pub fn saved_device_reappeared(saved: Option<&str>, previous: &[String], current: &[String], currently_bound: Option<&str>) -> bool`
  - `pub struct AvailableAudioDevices(pub Vec<String>)` (`Resource, Default, Debug, Clone`)
  - `pub struct BoundOutputDevice(pub Option<String>)` (`Resource, Default, Debug, Clone`)
  - `#[cfg(not(target_arch = "wasm32"))] pub(crate) fn enumerate_output_names(host: &cpal::Host) -> Vec<String>`

**Why pure (the two functions).** Name resolution and topology diff are the decisions this half turns on, and both are total functions over `&[String]`. They are unit-tested with literal lists — no host, no device. The `AvailableAudioDevices` resource feeds Plan 03a's dropdown (Task 7); `BoundOutputDevice` records which endpoint the live stream is on so the migrate-back check knows when a rebuild is worthwhile.

- [ ] **Step 1: Register the module and write the failing tests**

In `crates/wc-core/src/audio/mod.rs`, add `pub mod device;`. Then create `crates/wc-core/src/audio/device.rs` with **only** the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn saved_name_present_resolves_to_preferred() {
        let available = names(&["Built-in", "LG TV (HDMI)"]);
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &available),
            DeviceResolution::Preferred("LG TV (HDMI)".to_owned()),
        );
    }

    #[test]
    fn saved_name_absent_falls_back_but_keeps_the_name() {
        // The HDMI TV is merely asleep and not enumerated right now. We fall
        // back to the default so there is *some* sound, but we must remember
        // the operator's choice so a later migrate-back can restore it.
        let available = names(&["Built-in"]);
        assert_eq!(
            resolve_output_device(Some("LG TV (HDMI)"), &available),
            DeviceResolution::Fallback {
                saved_unavailable: Some("LG TV (HDMI)".to_owned()),
            },
        );
    }

    #[test]
    fn no_saved_name_or_empty_falls_back_with_no_regret() {
        let available = names(&["Built-in"]);
        assert_eq!(
            resolve_output_device(None, &available),
            DeviceResolution::Fallback { saved_unavailable: None },
        );
        // An empty stored string is "no choice", not a device literally named "".
        assert_eq!(
            resolve_output_device(Some(""), &available),
            DeviceResolution::Fallback { saved_unavailable: None },
        );
    }

    #[test]
    fn reappearance_is_true_only_on_the_rising_edge_when_not_already_bound() {
        let saved = Some("LG TV (HDMI)");
        let without = names(&["Built-in"]);
        let with = names(&["Built-in", "LG TV (HDMI)"]);

        // Rising edge: absent last poll, present now, and we are on the fallback.
        assert!(saved_device_reappeared(saved, &without, &with, Some("Built-in")));
        // Steady presence (was already there) is not an edge.
        assert!(!saved_device_reappeared(saved, &with, &with, Some("Built-in")));
        // Already bound to the saved device: nothing to migrate.
        assert!(!saved_device_reappeared(saved, &without, &with, Some("LG TV (HDMI)")));
        // No saved preference: never migrate.
        assert!(!saved_device_reappeared(None, &without, &with, Some("Built-in")));
        // Still absent: not an edge.
        assert!(!saved_device_reappeared(saved, &without, &without, Some("Built-in")));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib audio::device 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function resolve_output_device`, `cannot find type DeviceResolution`, etc.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/wc-core/src/audio/device.rs`, above the test module:

```rust
//! Output-device enumeration, name resolution, and topology diffing.
//!
//! ## What runs where
//!
//! [`resolve_output_device`] and [`saved_device_reappeared`] are **pure** — no
//! host, no device, no thread — and carry the two decisions this half turns on,
//! so they are unit-tested with literal name lists (CI has no audio device).
//!
//! [`enumerate_output_names`] calls into cpal and **can block** (WASAPI
//! enumeration in particular). It is therefore only ever called from (a) the
//! one-shot startup path and event-driven rebuilds on the **main thread**, and
//! (b) the device-watcher OS thread added in Task 4 — never the audio callback
//! and never the render thread. On WASAPI, cpal initialises COM per-thread
//! internally (`com::com_initialized()` runs at the top of every device
//! operation), so calling this from a freshly spawned watcher thread is sound
//! without any manual `CoInitializeEx`.

// Transient. `resolve_output_device` and `saved_device_reappeared` have only
// test callers, and `AvailableAudioDevices` / `BoundOutputDevice` /
// `enumerate_output_names` are consumed by the watcher (Task 4) and the
// supervisor (Task 5). Until those land, the lib target sees them as dead code
// and `clippy -D warnings` fails. Task 5 removes this attribute and verifies
// clippy stays clean without it.
#![allow(dead_code)]

use bevy::prelude::Resource;

/// The chosen output device after matching a saved name against the live list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceResolution {
    /// The saved name matched a live device; open it by name.
    Preferred(String),
    /// No usable saved name, or the saved name is not currently enumerated;
    /// open the host's default output device. `saved_unavailable` carries the
    /// name we are *keeping persisted* while falling back (e.g. a sleeping HDMI
    /// TV) so a later migrate-back can restore it, or `None` when the operator
    /// expressed no preference.
    Fallback {
        /// The saved-but-currently-absent device name, preserved for logging
        /// and for the migrate-back check. `None` when nothing was saved.
        saved_unavailable: Option<String>,
    },
}

/// Match a saved device name against the live output-device list.
///
/// An empty saved string is treated as "no preference" (the sentinel the
/// settings field uses for "system default"), never as a device literally named
/// `""`. A saved name that is not in `available` yields a
/// [`DeviceResolution::Fallback`] that **keeps the name** — resolution never
/// silently rewrites the operator's choice.
#[must_use]
pub fn resolve_output_device(saved: Option<&str>, available: &[String]) -> DeviceResolution {
    match saved {
        Some(name) if !name.is_empty() && available.iter().any(|d| d == name) => {
            DeviceResolution::Preferred(name.to_owned())
        }
        Some(name) if !name.is_empty() => DeviceResolution::Fallback {
            saved_unavailable: Some(name.to_owned()),
        },
        _ => DeviceResolution::Fallback {
            saved_unavailable: None,
        },
    }
}

/// Whether a rebuild should be triggered to *migrate back* to the saved device.
///
/// True only on the rising edge: the saved endpoint is in `current` but was not
/// in `previous` (it just reappeared) **and** we are not already bound to it. A
/// missing or empty `saved`, or being already bound to it, yields `false`, so
/// steady-state polls never thrash the stream.
#[must_use]
pub fn saved_device_reappeared(
    saved: Option<&str>,
    previous: &[String],
    current: &[String],
    currently_bound: Option<&str>,
) -> bool {
    let Some(name) = saved.filter(|n| !n.is_empty()) else {
        return false;
    };
    if currently_bound == Some(name) {
        return false;
    }
    current.iter().any(|d| d == name) && !previous.iter().any(|d| d == name)
}

/// Live list of output-device names, refreshed by the device-watcher thread
/// (Task 4). Read by the audio settings panel (Task 7, via Plan 03a's
/// runtime-enumerated dropdown) and by the supervisor's migrate-back check.
///
/// Main-thread-only resource; the watcher thread never touches it directly (it
/// sends snapshots over a channel that a main-thread system drains into here).
#[derive(Resource, Default, Debug, Clone)]
pub struct AvailableAudioDevices(pub Vec<String>);

/// Name of the output device the live stream is currently bound to, or `None`
/// before the engine starts / when it failed to build.
///
/// Set on every successful (re)build (Task 5). The migrate-back check compares
/// against this so it does not rebuild a stream that is already on the saved
/// device.
#[derive(Resource, Default, Debug, Clone)]
pub struct BoundOutputDevice(pub Option<String>);

/// Enumerate the host's output devices and collect their names.
///
/// **Can block** (WASAPI). Only called on the main thread (startup / rebuild)
/// or the watcher thread — see the module header. A device whose name cannot be
/// read is skipped. Returns an empty vec if enumeration itself errors, which the
/// resolver treats as "nothing available -> fall back to default".
///
/// Allocates a `Vec<String>` (cpal returns owned names); this is forced by
/// cpal's API and is acceptable because it runs at most every ~2 s on a
/// background thread, never on the audio callback or a per-frame render system.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn enumerate_output_names(host: &cpal::Host) -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    match host.output_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(err) => {
            tracing::warn!(?err, "cpal output_devices enumeration failed");
            Vec::new()
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib audio::device`

Expected: PASS, 4 tests.

- [ ] **Step 5: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib audio::device
git add crates/wc-core/src/audio/device.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio/device): device-name resolution, topology diff, shared resources

resolve_output_device matches a saved device name against the live list and
falls back to the system default when it is absent -- keeping the saved name
persisted, never rewriting it (a sleeping HDMI TV must not lose its binding).
saved_device_reappeared fires only on the rising edge, so steady polling
never thrashes the stream. Both are pure and unit-tested with literal lists.

AvailableAudioDevices feeds the Plan 03a dropdown; BoundOutputDevice records
the live endpoint for the migrate-back check; enumerate_output_names is the
one blocking cpal call, confined to the main thread and the watcher thread.

Carries a transient #![allow(dead_code)] removed in Task 5.
EOF
git show --stat HEAD
```

---

### Task 4: Device-watcher thread and the topology-drain system

**Files:**
- Modify: `crates/wc-core/src/audio/device.rs` (add the watcher spawn, the channel resources, and `drain_device_topology`; extend `mod tests`)
- Modify: `crates/wc-core/src/audio/mod.rs` (spawn the watcher at startup; register resources; add `drain_device_topology` to `PreUpdate`)

**Interfaces:**
- Consumes: `enumerate_output_names`, `AvailableAudioDevices`, `BoundOutputDevice`, `saved_device_reappeared` (Task 3); `AudioSupervisor` (Task 2).
- Produces:
  - `#[cfg(not(target_arch = "wasm32"))] pub struct DeviceWatcher` (`Resource`) — owns the join handle + stop flag; `Drop` joins the thread.
  - `pub struct DeviceTopologyReceiver` — non-send wrapper over `mpsc::Receiver<Vec<String>>`.
  - `#[cfg(not(target_arch = "wasm32"))] pub fn spawn_device_watcher() -> (DeviceWatcher, DeviceTopologyReceiver)`
  - `pub fn drain_device_topology(...)` Bevy system.
  - `pub(crate) fn apply_topology(available: &mut Vec<String>, incoming: Vec<String>, saved: Option<&str>, bound: Option<&str>) -> bool` — pure core of the drain (returns whether a migrate-back is warranted).

**Thread map.** The watcher is a single OS thread that owns its own `cpal::Host` (built in-thread), sleeps in short increments, and every ~2 s enumerates output names and — only when the list *changed* — sends a snapshot over an `mpsc` channel. It checks a stop flag each increment so shutdown joins within ~100 ms. It **never** touches Bevy state, the audio callback, or the render thread. `drain_device_topology` runs on the **main thread** (`PreUpdate`), pulls snapshots off the channel, updates `AvailableAudioDevices`, and — via the pure `apply_topology` — decides whether the saved endpoint just reappeared, calling `AudioSupervisor::request_now` if so.

- [ ] **Step 1: Write the failing test for the pure drain core**

Add to `crates/wc-core/src/audio/device.rs`'s `mod tests`:

```rust
    #[test]
    fn apply_topology_updates_the_list_and_flags_reappearance() {
        let mut available = names(&["Built-in"]);
        // The saved HDMI TV reappears while we are on the fallback.
        let migrate = apply_topology(
            &mut available,
            names(&["Built-in", "LG TV (HDMI)"]),
            Some("LG TV (HDMI)"),
            Some("Built-in"),
        );
        assert!(migrate, "saved endpoint reappeared -> migrate back");
        assert_eq!(available, names(&["Built-in", "LG TV (HDMI)"]));
    }

    #[test]
    fn apply_topology_no_migrate_when_nothing_relevant_changed() {
        let mut available = names(&["Built-in", "LG TV (HDMI)"]);
        // Same list, already bound to the saved device: no migrate.
        let migrate = apply_topology(
            &mut available,
            names(&["Built-in", "LG TV (HDMI)"]),
            Some("LG TV (HDMI)"),
            Some("LG TV (HDMI)"),
        );
        assert!(!migrate);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib audio::device::tests::apply_topology 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function apply_topology`.

- [ ] **Step 3: Write the pure drain core plus the thread and resources**

Add to `crates/wc-core/src/audio/device.rs`, below `enumerate_output_names` and above the test module. First the pure core (unconditional, so tests run on every platform):

```rust
/// Apply an incoming topology snapshot to the live list and report whether the
/// saved endpoint just reappeared (so the caller should trigger a migrate-back).
///
/// Pure: the previous list is `available` before the swap, `incoming` is the
/// fresh snapshot. Compares them with [`saved_device_reappeared`] *before*
/// overwriting, then moves `incoming` into `available` (no clone).
#[must_use]
pub(crate) fn apply_topology(
    available: &mut Vec<String>,
    incoming: Vec<String>,
    saved: Option<&str>,
    bound: Option<&str>,
) -> bool {
    let migrate = saved_device_reappeared(saved, available, &incoming, bound);
    *available = incoming;
    migrate
}
```

Then the watcher thread and channel plumbing (native only):

```rust
/// The ~2 s cadence at which the watcher re-enumerates output devices.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Granularity at which the watcher wakes to check its stop flag, so app
/// shutdown joins the thread promptly instead of waiting a full interval.
#[cfg(not(target_arch = "wasm32"))]
const WATCH_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// Owns the device-watcher OS thread. Dropping it signals the thread to stop
/// and joins it, so the app exits cleanly. A Bevy `Resource`; Bevy drops it on
/// app teardown.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource)]
pub struct DeviceWatcher {
    /// Set to `true` to ask the thread to exit at its next tick.
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Join handle, taken on `Drop`.
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // A failed join means the watcher panicked; log rather than
            // propagate, since this runs during app teardown.
            if handle.join().is_err() {
                tracing::warn!("device-watcher thread panicked before join");
            }
        }
    }
}

/// Consumer end of the watcher -> main topology channel.
///
/// `mpsc::Receiver` is `Send` but not `Sync`, so like the audio rings it is
/// installed as a **non-send** resource and only ever read on the main thread.
pub struct DeviceTopologyReceiver {
    /// Receives a fresh name snapshot only when the list actually changed.
    rx: std::sync::mpsc::Receiver<Vec<String>>,
}

impl DeviceTopologyReceiver {
    /// Drain every snapshot the watcher has queued since the last tick, newest
    /// last. Returns the most recent snapshot, or `None` if nothing arrived.
    fn latest(&self) -> Option<Vec<String>> {
        let mut newest = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            newest = Some(snapshot);
        }
        newest
    }
}

/// Spawn the device-watcher thread. Returns the owning [`DeviceWatcher`]
/// resource and the [`DeviceTopologyReceiver`] the main thread drains.
///
/// The thread builds its own `cpal::Host` in-thread (hosts are not moved across
/// threads — cpal inits COM per-thread on WASAPI), enumerates every
/// [`WATCH_INTERVAL`], and pushes a snapshot **only when the list changed** so
/// the channel stays quiet in steady state. It reuses one `last` buffer across
/// iterations; the only per-change allocation is the snapshot it must hand off,
/// which is forced by the channel boundary and occurs only on real topology
/// changes, not per poll.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn spawn_device_watcher() -> (DeviceWatcher, DeviceTopologyReceiver) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();

    let handle = std::thread::Builder::new()
        .name("wc-audio-device-watcher".to_owned())
        .spawn(move || {
            let host = cpal::default_host();
            let mut last: Vec<String> = enumerate_output_names(&host);
            // Publish the initial list so the dropdown and resolver see it
            // without waiting a full interval.
            if tx.send(last.clone()).is_err() {
                return; // main side already gone
            }
            let mut since_poll = std::time::Duration::ZERO;
            while !stop_thread.load(Ordering::Relaxed) {
                std::thread::sleep(WATCH_TICK);
                since_poll += WATCH_TICK;
                if since_poll < WATCH_INTERVAL {
                    continue;
                }
                since_poll = std::time::Duration::ZERO;
                let current = enumerate_output_names(&host);
                if current != last {
                    if tx.send(current.clone()).is_err() {
                        return; // main side dropped the receiver
                    }
                    last = current;
                }
            }
        });

    match handle {
        Ok(handle) => (
            DeviceWatcher {
                stop,
                handle: Some(handle),
            },
            DeviceTopologyReceiver { rx },
        ),
        Err(err) => {
            // Could not spawn the watcher: return a stopped shell and a dead
            // receiver. The app still runs (recovery just cannot see topology
            // changes); startup enumeration on the main thread still works.
            tracing::warn!(?err, "failed to spawn device-watcher thread");
            (
                DeviceWatcher {
                    stop,
                    handle: None,
                },
                DeviceTopologyReceiver { rx },
            )
        }
    }
}
```

Then the main-thread drain system (native only — it references the watcher resources; the pure `apply_topology` is what tests exercise):

```rust
/// `PreUpdate` system: pull the latest topology snapshot off the watcher
/// channel, update [`AvailableAudioDevices`], and trigger a migrate-back rebuild
/// when the saved endpoint just reappeared.
///
/// Runs on the **main thread** (`DeviceTopologyReceiver` is non-send). It never
/// enumerates — the blocking enumeration already happened on the watcher thread;
/// this only moves an already-built `Vec<String>` into a resource.
///
/// `saved` is `None` in the recovery-only stage (Task 6 wires the persisted
/// `AudioSettings::output_device` in), so migrate-back is inert until a device
/// can be chosen — recovery to the default is the behaviour until then.
#[cfg(not(target_arch = "wasm32"))]
pub fn drain_device_topology(
    receiver: Option<bevy::ecs::system::NonSend<'_, DeviceTopologyReceiver>>,
    mut available: ResMut<'_, AvailableAudioDevices>,
    bound: Res<'_, BoundOutputDevice>,
    mut supervisor: ResMut<'_, crate::audio::supervisor::AudioSupervisor>,
    time: Res<'_, Time<Real>>,
) {
    let Some(receiver) = receiver else {
        return;
    };
    let Some(incoming) = receiver.latest() else {
        return;
    };
    // Task 6 replaces this `None` with the persisted device name.
    let saved: Option<&str> = None;
    let migrate = apply_topology(
        &mut available.0,
        incoming,
        saved,
        bound.0.as_deref(),
    );
    if migrate {
        supervisor.request_now(time.elapsed_secs_f64());
    }
}
```

Add the imports this needs at the top of `device.rs` (merge with the existing `use bevy::prelude::Resource;`):

```rust
use bevy::prelude::{Res, ResMut, Resource};
#[cfg(not(target_arch = "wasm32"))]
use bevy::prelude::{Real, Time};
```

> **Hot-path note to leave in the code:** `receiver.latest()` collapses any backlog to the newest snapshot and returns early when nothing arrived, so `drain_device_topology` allocates nothing in steady state (the common case is `None`). The watcher's per-change `clone()` is the one forced allocation, and it fires only on a real topology change.

- [ ] **Step 4: Wire the watcher into `AudioPlugin`**

In `crates/wc-core/src/audio/mod.rs`, register the resources and system, and spawn the watcher. In `AudioPlugin::build`, add the resource inits and the `PreUpdate` system (native-gated), after the existing `.init_resource::<AudioState>()`:

```rust
            .init_resource::<AudioState>()
            .init_resource::<device::AvailableAudioDevices>()
            .init_resource::<device::BoundOutputDevice>()
            .init_resource::<supervisor::AudioSupervisor>()
            .add_systems(Startup, engine::start_audio_engine)
            .add_systems(PreUpdate, state::pump_audio_messages)
```

and, still inside `build`, gated for native:

```rust
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(PreUpdate, device::drain_device_topology.after(state::pump_audio_messages));
```

Spawn the watcher inside `engine::start_audio_engine` (it already has `world: &mut World`). At the end of the `Ok(built) => { … }` arm in `start_audio_engine` (`engine.rs:87-106`), add, native-gated:

```rust
            #[cfg(not(target_arch = "wasm32"))]
            {
                let (watcher, topology_rx) = super::device::spawn_device_watcher();
                world.insert_resource(watcher);
                world.insert_non_send(topology_rx);
            }
```

(Import note: `start_audio_engine` already takes `world: &mut World`; `insert_resource` / `insert_non_send` are in scope.)

- [ ] **Step 5: Run the tests and the gate**

Run: `cargo test -p wc-core --lib audio::device`

Expected: PASS, 6 tests (4 from Task 3 + the 2 `apply_topology` tests).

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
git add crates/wc-core/src/audio/device.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio/device): device-watcher thread + main-thread topology drain

A single OS thread owns its own cpal::Host, enumerates output devices every
~2 s off both the audio callback and the render thread (WASAPI enumeration
can block), and sends a name snapshot only when the list changes. It wakes
every 100 ms to check a stop flag so app shutdown joins it promptly.
drain_device_topology (PreUpdate, main thread, non-send receiver) moves the
newest snapshot into AvailableAudioDevices and, via the pure apply_topology,
requests an immediate migrate-back when the saved endpoint reappears. saved
is None until Task 6 wires the persisted setting in.
EOF
git show --stat HEAD
```

- [ ] **Step 6: Human smoke check (optional but recommended)**

Run: `cargo rund`. With the app running, plug or unplug a USB/HDMI audio output (or sleep/wake an HDMI display). The log should show the watcher's snapshot changing (add a temporary `tracing::info!` in `drain_device_topology` if you want to see it, then remove it). No behaviour change is expected yet — Task 5 acts on the topology.

---

### Task 5: Engine rebuild and the `supervise_audio` system (recovery goes live)

**Files:**
- Modify: `crates/wc-core/src/audio/engine.rs` (retain sample assets; factor `open_output_device`; add `rebuild_engine`; set `BoundOutputDevice` on build)
- Create: `crates/wc-core/src/audio/supervisor.rs` — add the `supervise_audio` system to the existing file; **remove** its transient `#![allow(dead_code)]`
- Modify: `crates/wc-core/src/audio/device.rs` — **remove** its transient `#![allow(dead_code)]`
- Modify: `crates/wc-core/src/audio/mod.rs` (register `supervise_audio`)

**Interfaces:**
- Consumes: `AudioSupervisor`, `SupervisorAction` (Task 2); `resolve_output_device`, `DeviceResolution`, `enumerate_output_names`, `BoundOutputDevice` (Task 3); `AudioStatus::Reconnecting`, `mark_reconnecting_from_callback` (Task 1); `build_engine`, `AudioStream`, `AudioErrorFlag` (engine).
- Produces:
  - `#[cfg(not(target_arch = "wasm32"))] pub(crate) fn open_output_device(host: &cpal::Host, resolution: &DeviceResolution) -> Result<(cpal::Device, String), EngineBuildError>`
  - `#[cfg(not(target_arch = "wasm32"))] pub(crate) fn rebuild_engine(world: &mut World)` (the actual stream swap)
  - `#[cfg(not(target_arch = "wasm32"))] pub fn supervise_audio(world: &mut World)` Bevy system.

**Thread map.** `supervise_audio` is an **exclusive main-thread system** (`&mut World`). It reads `Time<Real>` for the clock, observes `AudioStatus`, and drives the `AudioSupervisor`. On a due attempt it calls `rebuild_engine`, which enumerates on the **main thread** (event-driven, not per-frame) to resolve the device, builds a fresh stream + rings + error flag via `build_engine`, swaps the non-send resources, sets `BoundOutputDevice`, and restores play/pause from `AppState`. Nothing here runs on the audio callback or the render thread. The rebuild's blocking enumeration is a one-off reconnect cost, not a steady-state one.

- [ ] **Step 1: Retain the sample assets and factor device opening in `engine.rs`**

`start_audio_engine` currently *consumes* `SampleAssets` (`engine.rs:84`, `world.remove_resource::<SampleAssets>()`), so a rebuild has nothing to decode from. Change it to **read without removing** so rebuilds can reuse it. Replace `engine.rs:84`:

```rust
    // Read (do not remove) the encoded sample assets so a later stream rebuild
    // can re-decode them. `get_resource` is present-or-default; the binary
    // crate inserts the real assets before Startup. Retaining the *encoded*
    // (compressed) bytes is a small memory cost that buys mid-run reconnect.
    let assets = world
        .get_resource::<SampleAssets>()
        .cloned()
        .unwrap_or_default();
```

(`SampleAssets` derives `Default` and is a `Resource`; confirm it is `Clone` — if it is not, add `#[derive(Clone)]` to it in `audio/background.rs` in this step, since the rebuild path needs to re-read it. Verify: `rg -n "struct SampleAssets" crates/wc-core/src/audio/background.rs` and check its derives.)

Now factor device opening. `build_engine` (`engine.rs:138-146`) currently hard-codes `host.default_output_device()`. Change its signature to accept a resolution and add the helper. Replace `engine.rs:138-146` (the `fn build_engine(...)` opening through `let config: cpal::StreamConfig = supported.into();`):

```rust
fn build_engine(
    assets: &SampleAssets,
    resolution: &super::device::DeviceResolution,
) -> Result<BuiltEngine, EngineBuildError> {
    let host = cpal::default_host();
    let (device, device_name) = open_output_device(&host, resolution)?;
    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    let config: cpal::StreamConfig = supported.into();
```

Then extend `BuiltEngine` (`engine.rs:115-124`) with the resolved name so callers can record `BoundOutputDevice`:

```rust
struct BuiltEngine {
    stream: AudioStream,
    sender: AudioCommandSender,
    receiver: AudioMessageReceiver,
    /// Set by the cpal error callback when the stream dies mid-run; read by
    /// `pump_audio_messages`. Wrapped in [`AudioErrorFlag`] at install time.
    error_flag: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    /// Name of the output device this stream is bound to. Recorded into
    /// `BoundOutputDevice` so the migrate-back check knows the live endpoint.
    device_name: String,
}
```

and populate it in the `Ok(BuiltEngine { … })` return (`engine.rs:247-254`) by adding `device_name,` to the struct literal.

Add the `open_output_device` helper, placed above `build_engine`:

```rust
/// Resolve a [`DeviceResolution`] to a concrete cpal device plus its name.
///
/// `Preferred(name)` searches the host's output devices for an exact name
/// match; if the name has vanished since it was resolved (a race with a device
/// blip), it falls through to the default rather than erroring. `Fallback`
/// opens the host default. Errors only when there is *no* output device at all
/// (`NoDefaultDevice`), which is the genuinely unrecoverable case.
///
/// **Can block** (enumeration). Called only on the main thread (startup /
/// rebuild), never the audio callback or the render thread.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn open_output_device(
    host: &cpal::Host,
    resolution: &super::device::DeviceResolution,
) -> Result<(cpal::Device, String), EngineBuildError> {
    use cpal::traits::{DeviceTrait, HostTrait};
    if let super::device::DeviceResolution::Preferred(name) = resolution {
        if let Ok(mut devices) = host.output_devices() {
            if let Some(device) = devices.find(|d| d.name().is_ok_and(|n| &n == name)) {
                return Ok((device, name.clone()));
            }
        }
        tracing::warn!(device = %name, "saved output device not found; using default");
    }
    let device = host
        .default_output_device()
        .ok_or(EngineBuildError::NoDefaultDevice)?;
    let name = device.name().unwrap_or_else(|_| "default".to_owned());
    Ok((device, name))
}
```

Update the **existing** `start_audio_engine` call site of `build_engine` (`engine.rs:86`, `match build_engine(&assets)`). In the recovery-only stage the resolution is the default (no saved name yet — Task 6 supplies it). Replace `engine.rs:86`:

```rust
    // Recovery stage: no persisted device name yet (Task 6 wires it in), so
    // resolve to the system default. Enumeration here is a one-shot main-thread
    // cost at startup.
    let host_names = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            super::device::enumerate_output_names(&cpal::default_host())
        }
        #[cfg(target_arch = "wasm32")]
        {
            Vec::new()
        }
    };
    let resolution = super::device::resolve_output_device(None, &host_names);
    match build_engine(&assets, &resolution) {
```

and in that `Ok(built) => { … }` arm, record the bound device (after the existing `insert_non_send` calls, before the watcher spawn added in Task 4):

```rust
            world.resource_mut::<super::device::BoundOutputDevice>().0 = Some(built.device_name.clone());
```

- [ ] **Step 2: Add `rebuild_engine` and `supervise_audio` (write the failing test first)**

The rebuild's stream swap cannot be unit-tested without a device (CI has none). The **decision** logic is already covered by Task 2's `AudioSupervisor` tests. Add one more pure test that pins the status→action wiring `supervise_audio` relies on, so the wiring itself is guarded. Append to `crates/wc-core/src/audio/supervisor.rs`'s `mod tests`:

```rust
    #[test]
    fn a_fresh_reconnecting_status_begins_a_cycle_then_becomes_due() {
        // Mirrors the branch in supervise_audio: status just became
        // Reconnecting and no cycle is scheduled yet -> begin at `now`.
        let mut sup = AudioSupervisor::default();
        assert!(!sup.is_reconnecting());
        sup.begin(10.0);
        assert_eq!(sup.poll(10.0 + 0.9), SupervisorAction::Idle);
        assert_eq!(sup.poll(10.0 + 1.0), SupervisorAction::Rebuild);
    }
```

- [ ] **Step 3: Run the test to verify it fails, then passes trivially**

Run: `cargo test -p wc-core --lib audio::supervisor`

Expected: this new test PASSES immediately (it uses only Task 2 API). It exists to lock the begin→poll contract that `supervise_audio` depends on; if a later edit changes `begin`'s scheduling, this fails. (No red step here — this is a characterization test over already-passing code. The real rebuild is verified by the human step below.)

- [ ] **Step 4: Write `supervise_audio` and `rebuild_engine`**

Add to `crates/wc-core/src/audio/supervisor.rs`, above the test module (native only):

```rust
/// Main-thread exclusive system that drives reconnection.
///
/// Runs each `Update`. Reads the monotonic clock from `Time<Real>`, observes
/// [`AudioStatus`], and:
///
/// 1. On the edge into `Reconnecting` with no cycle scheduled, calls
///    [`AudioSupervisor::begin`].
/// 2. Also treats a stream that never started (`NotStarted`/`Errored` with no
///    live `AudioStream`) as reconnectable, so a kiosk that boots before its TV
///    is awake eventually acquires audio.
/// 3. When [`AudioSupervisor::poll`] returns `Rebuild`, calls
///    [`crate::audio::engine::rebuild_engine`]; on a live stream afterward,
///    records success, else records the failure and lets the backoff grow.
///
/// Never runs on the audio callback or the render thread. The rebuild's
/// blocking enumeration is an event-driven reconnect cost, not per-frame.
#[cfg(not(target_arch = "wasm32"))]
pub fn supervise_audio(world: &mut bevy::prelude::World) {
    use crate::audio::state::{AudioState, AudioStatus};

    let now = world
        .resource::<bevy::prelude::Time<bevy::prelude::Real>>()
        .elapsed_secs_f64();

    let status = world.resource::<AudioState>().status;
    let has_stream = world.get_non_send_resource::<crate::audio::engine::AudioStream>().is_some();

    // Decide whether a cycle should be running.
    let wants_reconnect = matches!(status, AudioStatus::Reconnecting)
        || (!has_stream && matches!(status, AudioStatus::NotStarted | AudioStatus::Errored));

    {
        let mut sup = world.resource_mut::<AudioSupervisor>();
        if wants_reconnect {
            if !sup.is_reconnecting() {
                sup.begin(now);
            }
        } else {
            // Healthy: make sure no stale cycle lingers.
            sup.record_success();
        }
    }

    if !wants_reconnect {
        return;
    }

    let action = world.resource::<AudioSupervisor>().poll(now);
    if action != SupervisorAction::Rebuild {
        return;
    }

    crate::audio::engine::rebuild_engine(world);

    // Judge the outcome by whether a live stream now exists and the status is
    // no longer stuck in a failure state.
    let recovered = world
        .get_non_send_resource::<crate::audio::engine::AudioStream>()
        .is_some();
    let mut sup = world.resource_mut::<AudioSupervisor>();
    if recovered {
        sup.record_success();
    } else {
        sup.record_failure(now);
    }
}
```

The `is_reconnecting` / `poll` accessors are `#[cfg(test)]` today; `supervise_audio` needs `is_reconnecting` at runtime. **Remove the `#[cfg(test)]` gate** from `AudioSupervisor::is_reconnecting` (make it a plain `#[must_use] pub fn`) since it now has a production caller; keep `attempts` test-only. Update its doc to drop "test-only".

Add `rebuild_engine` to `crates/wc-core/src/audio/engine.rs`, above `build_engine` (native only):

```rust
/// Rebuild the cpal stream after a mid-run death (or first acquisition), swap
/// the engine resources, and restore play/pause from [`AppState`].
///
/// Exclusive main-thread access. Re-resolves the device (Task 6 supplies the
/// saved name; until then, default), builds a fresh stream + rings + error
/// flag, and replaces the non-send `AudioStream`, `AudioCommandSender`,
/// `AudioMessageReceiver`, and the `AudioErrorFlag`. Inserting the new
/// `AudioStream` drops the old one, stopping the dead stream. On failure it
/// leaves the old (dead) resources absent and logs; the supervisor retries on
/// backoff.
///
/// Restores play/pause from `AppState`: paused at `Home`, playing in any
/// sketch. Re-instantiating the active sketch's synth voice graph is a known
/// gap (see the plan's open questions) — this restores the stream and its
/// transport, not the DSP graph.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn rebuild_engine(world: &mut World) {
    use crate::lifecycle::state::AppState;

    let assets = world
        .get_resource::<SampleAssets>()
        .cloned()
        .unwrap_or_default();

    // Task 6 replaces this `None` with the persisted device name.
    let host_names = super::device::enumerate_output_names(&cpal::default_host());
    let resolution = super::device::resolve_output_device(None, &host_names);

    let built = match build_engine(&assets, &resolution) {
        Ok(built) => built,
        Err(err) => {
            tracing::warn!(?err, "audio stream rebuild failed; will retry on backoff");
            return;
        }
    };

    let device_name = built.device_name.clone();
    world.insert_non_send(built.sender);
    world.insert_non_send(built.receiver);
    world.insert_non_send(built.stream);
    world.insert_resource(AudioErrorFlag(built.error_flag));
    world.resource_mut::<super::device::BoundOutputDevice>().0 = Some(device_name.clone());
    {
        let mut state = world.resource_mut::<AudioState>();
        state.sample_rate = built.sample_rate;
        state.channels = built.channels;
        state.status = super::state::AudioStatus::Running;
        state.last_error = None;
    }

    // Restore transport from AppState. build_engine leaves the stream paused
    // (home-silence guarantee); resume only if a sketch is active.
    let in_sketch = world.resource::<State<AppState>>().get().is_sketch();
    if in_sketch {
        if let Some(stream) = world.get_non_send_resource::<AudioStream>() {
            stream.play();
        }
    }
    tracing::info!(device = %device_name, in_sketch, "audio stream rebuilt");
}
```

Add the needed imports to `engine.rs`: `use bevy::prelude::State;` is available via `bevy::prelude::*` (already `use bevy::prelude::*;` at `engine.rs:18`). Confirm `State<AppState>` resolves; `AppState` is imported via the `use crate::lifecycle::state::AppState;` inside `rebuild_engine`.

- [ ] **Step 5: Register `supervise_audio` and remove the transient allows**

In `crates/wc-core/src/audio/mod.rs`, add the system (native-gated), ordered after the pump so it sees the freshest status:

```rust
        #[cfg(not(target_arch = "wasm32"))]
        app.add_systems(Update, supervisor::supervise_audio.after(state::pump_audio_messages));
```

Remove the transient `#![allow(dead_code)]` from **both** `crates/wc-core/src/audio/supervisor.rs` and `crates/wc-core/src/audio/device.rs` (the six-ish comment lines plus the attribute). Every item now has a non-test caller:

```bash
rg -n "allow\(dead_code\)" crates/wc-core/src/audio/supervisor.rs crates/wc-core/src/audio/device.rs
# expect: no matches
```

If clippy then reports `dead_code` on any item, its production caller is missing — fix the wiring (Steps 1, 4, 5), do **not** restore the attribute.

- [ ] **Step 6: Run the full gate**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
```

Expected: all pass; `rg` for the dead-code allow prints nothing.

- [ ] **Step 7: Human verification — the recovery half actually recovers**

This cannot be unit-tested (no CI audio device). Run: `cargo rund`. Then:

1. Navigate into a sketch that makes sound (e.g. Line) and confirm audio plays.
2. Trigger a mid-run stream death: on macOS, switch the system output device (or unplug/replug a USB DAC / sleep-wake an HDMI display); on Windows, put the HDMI TV to sleep or switch its input.
3. Watch the log: it should show `Entering Reconnecting`, then `audio stream rebuilt device=… in_sketch=true` within the backoff window (≤ ~1–2 s for the first attempt), and sound should return **without restarting the app**.
4. Leave a device unplugged and confirm the retry backs off (1 s, 2 s, 4 s, …, capped 30 s) rather than busy-looping, then replug and confirm it re-acquires promptly (the watcher's topology change should trigger an early attempt).

Note in your report whether audio returned **audible** or merely `Running`-but-silent — if silent, that is the synth-reactivation gap in the open questions, not a rebuild failure.

- [ ] **Step 8: Commit**

```bash
git add crates/wc-core/src/audio/engine.rs crates/wc-core/src/audio/supervisor.rs crates/wc-core/src/audio/device.rs crates/wc-core/src/audio/mod.rs
git commit -F - <<'EOF'
feat(audio): supervisor rebuilds the stream on a backoff; recovery goes live

supervise_audio (main-thread exclusive system) drives reconnection: on the
edge into Reconnecting -- or a stream that never started -- it begins a
backoff cycle and, when an attempt is due, calls rebuild_engine. The rebuild
re-resolves the device (default for now; Task 6 adds the saved name),
recreates the stream/rings/error-flag, swaps the non-send resources,
records BoundOutputDevice, and restores play/pause from AppState. Blocking
enumeration stays on the main thread on the event-driven reconnect path,
never the audio callback or the render thread.

start_audio_engine now retains the encoded SampleAssets so a rebuild can
re-decode them (a small memory cost that buys mid-run reconnect). Removes the
transient dead-code allows from supervisor.rs and device.rs now that every
item has a production caller.

Known gap (see plan open questions): rebuild restores the stream transport,
not the active sketch's synth voice graph.
EOF
git show --stat HEAD
```

---

## UI half — BLOCKED ON PLAN 03a

> Tasks 6 and 7 must not start until Plan 03a (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`, Plan 03a entry) has merged its **runtime-enumerated setting widget**. That widget is the only way to render a dropdown whose options are discovered at runtime; `SettingKind::Enum` (`settings/def.rs:54-60`) is a **compile-time** `&'static [&'static str]` list filled by the derive macro from `TypeInfo`, and cpal discovers devices at runtime. This plan **consumes** 03a's widget; it does not design or build it.

### Task 6: `AudioSettings` — persist the output device by name; wire it into the resolver

**Files:**
- Create: `crates/wc-core/src/audio/settings.rs`
- Modify: `crates/wc-core/src/audio/mod.rs` (add `pub mod settings;`; register the section; pass the saved name into the resolver in three places)
- Modify: `crates/wc-core/src/audio/engine.rs` (read the saved name in `start_audio_engine` and `rebuild_engine`)
- Modify: `crates/wc-core/src/audio/device.rs` (read the saved name in `drain_device_topology`)

**Interfaces:**
- **Consumes (Plan 03a):** the runtime-enumerated widget. Its contract, which this task writes against:
  - the stored value is a `String` (the device name);
  - the dropdown's options come from a Bevy `Resource` the panel reads — here `device::AvailableAudioDevices(Vec<String>)`;
  - a saved name absent from the live list is **shown, marked unavailable, and kept persisted** — never silently rewritten (a sleeping HDMI TV must not lose its binding).
  - The exact `SettingKind` variant / attribute spelling 03a introduces (e.g. `ty = RuntimeEnum` plus a way to name the options resource) is defined by Plan 03a. Use whatever 03a shipped; do **not** invent it here. If 03a's attribute differs from the placeholder below, adjust the `#[setting(...)]` line to match 03a and leave the field type (`String`) and semantics unchanged.
- Produces:
  - `pub struct AudioSettings { pub output_device: String }` (`SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq`), `storage_key = "audio"`, section `"Audio"`.

**Design.** `output_device` is a `String`; the empty string is the "system default / follow OS" sentinel (the resolver already maps `Some("")` and `None` alike to `Fallback`). Persistence needs no new machinery — it is a TOML string like `Text`. Registering the section in `AudioPlugin::build` inserts the resource before `Startup`, so `start_audio_engine` can read it.

- [ ] **Step 1: Write the failing tests**

Create `crates/wc-core/src/audio/settings.rs` with the struct's tests (modeled on `hand_tracking.rs`):

```rust
#[cfg(test)]
#[allow(clippy::expect_used, reason = "expect is appropriate in test code")]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_empty_meaning_system_default() {
        let s = AudioSettings::default();
        assert!(s.output_device.is_empty(), "empty = follow the system default");
    }

    #[test]
    fn output_device_persists_as_the_name_string() {
        let s = AudioSettings {
            output_device: "LG TV (HDMI)".to_owned(),
        };
        let text = toml::to_string(&s).expect("serialize");
        assert!(text.contains("output_device = \"LG TV (HDMI)\""), "got: {text}");
    }

    #[test]
    fn a_saved_name_round_trips_and_survives_an_absent_field() {
        let s = AudioSettings {
            output_device: "LG TV (HDMI)".to_owned(),
        };
        let text = toml::to_string(&s).expect("serialize");
        let back: AudioSettings = toml::from_str(&text).expect("parse back");
        assert_eq!(back.output_device, "LG TV (HDMI)");
        // A config saved before this field existed loads as the default.
        let old: AudioSettings = toml::from_str("").expect("empty config loads");
        assert!(old.output_device.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib audio::settings 2>&1 | head -20`

Expected: FAIL to compile — `cannot find type AudioSettings in this scope`.

- [ ] **Step 3: Write the struct**

Prepend to `crates/wc-core/src/audio/settings.rs`:

```rust
//! Audio-engine settings, persisted across sessions.
//!
//! One field today: the output device, stored **by name**. Empty means "follow
//! the system default". The settings panel renders it with Plan 03a's
//! runtime-enumerated dropdown, whose options come from
//! [`crate::audio::device::AvailableAudioDevices`]. A saved name that is not in
//! the live list is shown and kept, never rewritten (a sleeping HDMI TV must
//! keep its binding).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Global audio settings (not per-sketch).
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "audio")]
pub struct AudioSettings {
    /// Output device name. Empty = follow the system default output. When set,
    /// the engine opens the matching device at startup and after every
    /// reconnect; if the name is not currently enumerated (e.g. an HDMI TV
    /// asleep) the engine falls back to the default **and keeps this value**,
    /// so the binding is restored when the device reappears.
    ///
    /// Rendered with Plan 03a's runtime-enumerated widget; options come from
    /// [`crate::audio::device::AvailableAudioDevices`]. Adjust the `ty = …`
    /// below to the attribute Plan 03a shipped.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options = crate::audio::device::AvailableAudioDevices,
        category = User,
        section = "Audio",
        label = "Audio output device"
    )]
    #[serde(default)]
    pub output_device: String,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            output_device: String::new(),
        }
    }
}
```

> **03a coupling:** the `ty = RuntimeEnum` and `options = …` lines are placeholders for whatever Plan 03a's derive macro accepts. If 03a named the attribute differently, change only those lines. The field type stays `String` and the `Default`/serde behaviour stays as written.

- [ ] **Step 4: Register the section and wire the saved name into all three resolver sites**

In `crates/wc-core/src/audio/mod.rs`, add `pub mod settings;`, bring the extension trait into scope (`use crate::settings::RegisterSketchSettingsExt;`), and register in `AudioPlugin::build`:

```rust
        app.register_sketch_settings::<settings::AudioSettings>();
```

Then replace the three `saved: Option<&str> = None` placeholders with the persisted value:

- In `engine.rs` `start_audio_engine`, before resolving, read the setting:

```rust
    let saved = world.resource::<super::settings::AudioSettings>().output_device.clone();
    let resolution = super::device::resolve_output_device(
        (!saved.is_empty()).then_some(saved.as_str()),
        &host_names,
    );
```

- In `engine.rs` `rebuild_engine`, `world` is available; read it the same way and pass it to `resolve_output_device` in place of `None`.

- In `device.rs` `drain_device_topology`, add `settings: Option<Res<'_, crate::audio::settings::AudioSettings>>` to the system params and derive `saved`:

```rust
    let saved_owned = settings.as_ref().map(|s| s.output_device.clone()).unwrap_or_default();
    let saved: Option<&str> = (!saved_owned.is_empty()).then_some(saved_owned.as_str());
```

(`Option<Res<…>>` degrades cleanly if the resource is somehow absent; it is present because `AudioPlugin::build` registers it.)

- [ ] **Step 5: Run the tests and the gate**

```bash
cargo test -p wc-core --lib audio::settings
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/audio/settings.rs crates/wc-core/src/audio/mod.rs crates/wc-core/src/audio/engine.rs crates/wc-core/src/audio/device.rs
git commit -F - <<'EOF'
feat(audio/settings): persist the output device by name; resolve at startup

New AudioSettings section (storage_key "audio") with output_device: String,
empty meaning "follow the system default". start_audio_engine, rebuild_engine,
and drain_device_topology now pass the saved name into resolve_output_device,
so the engine opens the chosen device at startup and after every reconnect,
and the watcher's migrate-back fires when that device reappears. An absent
saved name falls back to the default and is kept persisted, never rewritten.

The panel widget is Plan 03a's runtime-enumerated dropdown (Task 7).
EOF
git show --stat HEAD
```

---

### Task 7: Render the device picker with Plan 03a's widget

**Files:**
- Modify: whichever file Plan 03a designates for a runtime-enum widget row (its `panel_user/widgets.rs` change). This task adds **no** new widget; it relies on `AudioSettings`'s `ty = RuntimeEnum` field (Task 6) being rendered by 03a's generic path, fed by `AvailableAudioDevices`.

**Interfaces:**
- **Consumes (Plan 03a):** the generic runtime-enum rendering path, which reads the options `Resource` named in the `#[setting(options = …)]` attribute. This task's only job is to confirm the wiring end-to-end and handle the unresolvable-name presentation, per 03a's contract.
- Produces: nothing new in code — the value flows `AudioSettings.output_device` ⇄ persistence ⇄ resolver (all built in Task 6).

**Why this is thin.** Because 03a builds the widget and Task 6 declares the field with `ty = RuntimeEnum` + `options = AvailableAudioDevices`, the settings panel already renders the dropdown through 03a's generic path. This task is verification plus the presentation edge case (saved name not in the live list), which 03a's contract already specifies — confirm it holds for audio.

- [ ] **Step 1: Confirm the field renders and the options resource is populated**

Read Plan 03a's merged widget code to confirm the attribute spelling and options-resource lookup match what Task 6 wrote. If they differ, reconcile Task 6's `#[setting(...)]` line to 03a's actual attribute (field type stays `String`). Grep to confirm the section is registered and the resource exists:

```bash
rg -n "AudioSettings|AvailableAudioDevices|RuntimeEnum" crates/wc-core/src/settings crates/wc-core/src/audio
```

- [ ] **Step 2: Human verification — sound reaches the TV, and the binding survives a blip**

No CI test can cover this (no audio device). Run: `cargo rund`, open the user settings panel (the Audio section), and:

1. With an HDMI TV connected, confirm it appears in the "Audio output device" dropdown. Select it. Confirm sound moves to the TV **without restarting** (the setting change should re-resolve; if 03a's path does not re-open the stream on change, add a change-listener that calls the engine rebuild — note this as a finding, since re-open-on-change may belong here or in 03a's generic apply path).
2. Confirm the choice persists: quit, relaunch, and verify the TV is still selected and receiving audio.
3. Put the TV to sleep (or switch its input) so its endpoint drops. Confirm the dropdown still **shows the saved name, marked unavailable**, and that the persisted value is unchanged (inspect the saved TOML: `output_device = "…"` is still the TV). Wake the TV and confirm audio migrates back to it within a couple of seconds (the watcher's reappearance trigger).

- [ ] **Step 3: Commit (only if this task changed code)**

If Task 7 required a code change (e.g. a re-open-on-change listener, or reconciling the attribute), stage exactly those files and commit with `git commit -F`. If it was verification-only, there is nothing to commit — record the findings in your report instead.

```bash
# only if code changed:
git add <named files>
git commit -F - <<'EOF'
feat(audio): re-open the output stream when the device setting changes

<describe the change, e.g. a change-listener on AudioSettings that calls the
engine rebuild so a picker selection takes effect without a restart>
EOF
git show --stat HEAD
```

---

## Self-Review

**Locked decisions, each mapped to a task.**

| Locked decision | Where |
| --- | --- |
| Enumerate `host.output_devices()` | `enumerate_output_names` (Task 3); watcher + startup (Tasks 4, 5) |
| Persist the choice **by name** | `AudioSettings.output_device: String` (Task 6) |
| Resolve at startup with fallback to system default | `resolve_output_device` (Task 3), wired in `start_audio_engine` (Tasks 5, 6) |
| Supervisor replaces terminal `Errored` | `AudioStatus::Reconnecting` (Task 1); `supervise_audio` (Task 5) |
| Error-callback flag triggers rebuild with backoff 1/2/4…30 s | `backoff_delay` + `AudioSupervisor` (Task 2); `supervise_audio` (Task 5) |
| Background ~2 s topology poll; migrate back when saved endpoint reappears | watcher thread + `saved_device_reappeared` + `drain_device_topology` (Tasks 3, 4) |
| Rebuild re-resolves device, recreates stream, restores play/pause from `AppState` | `rebuild_engine` (Task 5) |
| `AudioStatus` gains `Reconnecting` | Task 1 |
| Enumeration/rebuild off the audio callback **and** the render thread | watcher thread (Task 4); main-thread event-driven rebuild (Task 5); documented per module |
| Audio thread contract unchanged (lock-free, no Mutex, no alloc after init) | untouched — the error callback still only stores one atomic; nothing new runs on it |

**Recovery before UI, 03a confined to the UI.** Tasks 1–5 (recovery) depend on nothing and land first; after Task 5 the install recovers with zero UI. The Plan 03a dependency appears only in Tasks 6–7 (header, and Task 6's Interfaces block), and only for the widget — the field type (`String`), persistence, and the resolver are all built in the recovery half and merely *read* by the UI.

**Per-path thread placement.** Audio callback: unchanged (one atomic store). Watcher thread: the only steady-state enumeration; owns its own host; never touches Bevy/render/audio-callback. Main thread: `drain_device_topology` (moves an already-built vec; allocates nothing in the common `None` case), `supervise_audio` + `rebuild_engine` (event-driven blocking enumeration on reconnect only). Nothing allocates on the audio callback; the one forced allocation (the watcher's per-change snapshot clone) is documented and fires only on real topology changes.

**No placeholders.** Every code step shows complete code. The two 03a-coupled spots (Task 6's `ty = RuntimeEnum` attribute and Task 7's rendering) are explicitly marked as consuming 03a's contract and say exactly what to adjust if 03a's spelling differs; they are dependencies, not TBDs.

**Type consistency (Produces ⇄ Consumes).** `resolve_output_device(Option<&str>, &[String]) -> DeviceResolution` is produced in Task 3 and consumed in `open_output_device`/`rebuild_engine`/`start_audio_engine` (Task 5) and `drain_device_topology` (Tasks 4/6). `AudioSupervisor::{begin,request_now,poll,record_failure,record_success}` (Task 2) are consumed by `supervise_audio` and `drain_device_topology`. `SupervisorAction` compared with `==` (derives `PartialEq, Eq`). `AvailableAudioDevices(Vec<String>)` / `BoundOutputDevice(Option<String>)` (Task 3) written by Tasks 4/5, read by 4/6/7. `AudioSettings.output_device: String` (Task 6) feeds `resolve_output_device` via `(!s.is_empty()).then_some(s.as_str())`.

**Clippy-rule scan of the example code.** No `.unwrap()`/`.expect()`/`panic!` outside `#[cfg(test)]` blocks (and those blocks carry the `#[allow(clippy::expect_used, reason=…)]` where used, as in `settings.rs`). No `assert_eq!(x.is_some(), true)` (all use `assert!`). No `0..(N+1)` ranges. `backoff_delay` uses `checked_shl(...).unwrap_or(u64::MAX)` (not `.unwrap()`). No `as` numeric casts (durations use `Duration::from_secs` / `as_secs_f64`). Transient `#![allow(dead_code)]` in `supervisor.rs` and `device.rs` each carry an explicit removal step in Task 5.

**Anchor verification (done while writing, against `v5-alpha`).** `engine.rs:141` is `.default_output_device()` inside `build_engine` — confirmed, matches the brief. `state.rs:185` is the line `Status set to Errored. Restart the app to recover audio.` inside the `tracing::error!` at `:183-186` — confirmed, matches the brief. `rg` confirms `default_output_device` at `engine.rs:141` is the **only** cpal device-acquisition call in `crates/` (the other `cpal::` hits are the `use`, error-type `#[from]`s, the callback signature, and doc comments). cpal is pinned at `0.16` (`Cargo.toml:106`).

## Open questions (could not be resolved by reading code; need a build or a human)

1. **Synth re-activation after reconnect.** The locked scope is "restore play/pause from `AppState`," which `rebuild_engine` does. But the new `DspHost` starts with no synth graph, and the sketches' `Add*Synth` commands fire only on `OnEnter`, which does not re-fire on reconnect — so a reconnect mid-sketch may yield `Running`-but-silent audio until the visitor navigates away and back. A minimal fix is to map the active `AppState` to its `Add{Sketch}Synth` command and push it after a successful rebuild (idempotent per `command.rs`), but per-parameter state would still be stale until the sketch's next param write. This is deliberately **not** implemented (it is beyond the locked "play/pause" decision and risks audible double-adds). The Task 5 human step asks the operator to report whether reconnected audio is audible or silent; decide then whether to add the `AppState → Add*Synth` push.

2. **Does a settings *change* re-open the stream?** Task 7 assumes selecting a device in the panel takes effect live. Whether Plan 03a's generic apply path re-opens the audio stream on change, or whether Plan 04 must add its own change-listener that calls `rebuild_engine`, cannot be determined without seeing 03a's merged code. Flagged in Task 7, Step 2.

3. **`SampleAssets` `Clone`.** `rebuild_engine` and the retained-startup path `.cloned()` the encoded assets. Task 5, Step 1 instructs verifying `SampleAssets` derives `Clone` and adding it if not — this needs a read of `audio/background.rs` at implementation time (the exact derive set was not confirmed here) and a compile to be sure.

4. **cpal `Stream`/`Device` portability of the rebuild.** Confirmed by reading vendored cpal 0.16: WASAPI `Device` is `Send+Sync` and cpal inits COM per-thread, so the watcher thread is sound; `Stream` is `!Send` on macOS, so the stream stays main-thread-owned (as today). The rebuild swap is main-thread-only for that reason. Not an open risk, but it depends on cpal keeping per-thread COM init — re-verify on any cpal bump.

5. **Behaviour on a device with a different sample rate after reconnect.** `rebuild_engine` rebuilds the sample bank at the new device's `sample_rate`/`channels` (via `build_engine` → `build_sample_bank`). Re-decoding on every backoff attempt during a long outage is wasteful (event-driven, not steady-state, so within the AGENTS rules, but worth measuring). Whether to cache decoded PCM keyed by sample rate is a profiling-gated follow-up, not in scope.
