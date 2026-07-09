# Window-Resize Invalidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Line, Dots, Flame, and Cymatics rebuild themselves at the new window extent after a resize (F11 fullscreen, monitor re-enumeration, startup scale-factor settle) — silently and instantly, never blacking out or cutting audio — and stop the settings dock from loading misplaced/oversized during the one-frame `bevy_egui` scale-factor lag.

**Architecture:** A new `wc-core` module debounces `WindowResized` **and** `WindowScaleFactorChanged` and emits a single `WindowResizeSettled` message 250 ms after the last event. Each sketch runs an **always-on** listener (gating internally on being the running sketch, exactly like its `restart_on_settings_change` sibling) that re-runs the sketch's spawn path by driving the existing sketch-reload overlay. The overlay gains a `ReloadReason`: a settings restart keeps its 200 ms fade + audio dip; a window resize is instant and silent (one black repaint frame nobody sees, no audio command), so it is safe on every kiosk boot and for a TV waking from sleep. Every window-size-derived resource (particle counts, the Cymatics sim grid) is reallocated at the new size without touching the spawn systems themselves. Separately, the settings dock derives its geometry from egui's `ctx.content_rect()` (points) instead of Bevy's `Window` (logical pixels), so the anchor and the layout speak the same units during the stale frame.

**Tech Stack:** Rust, Bevy 0.19, `bevy_egui` 0.40 (egui 0.34), wgpu.

## Global Constraints

Copied from `AGENTS.md` and Part 1 of `docs/superpowers/plans/2026-07-09-alpha5-program-index.md`. Every task's requirements implicitly include this section.

- **Never allocate in a hot path.** Per-frame Bevy systems, egui paint-callback hooks, the audio callback, and continuously-running worker threads. Pre-allocate at init and reuse (`vec.clear()` keeps capacity). The listeners in this plan are message-drain shells that no-op in one cheap branch when no event arrived; they must not allocate.
- **No `unwrap()` or `expect()` in non-test code** unless the panic is a documented invariant violation.
- **No `as` casts on numeric types** where `From` / `TryFrom` would work.
- `///` rustdoc on **every** public item (struct, enum, trait, fn, const, module). Module-level `//!` on every module root.
- **Never strip comments during refactors.** Update stale comments; do not delete them.
- Public API at the top, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the file footer.
- One concept per file. ~300 lines is a guideline, not a hard cap.

**The per-task clippy gate MUST use `--all-targets`.** `cargo clippy -p <crate> --lib` skips the test target; CI runs `--all-targets`. Always:

```bash
cargo clippy -p <crate> --all-targets --all-features -- -D warnings
```

**Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** In your own example/test code:

- **`.expect()` / `.unwrap()`** are denied (`expect_used` / `unwrap_used`) in non-test code always, and in `#[cfg(test)]` code **unless** the test module carries `#[allow(clippy::expect_used, reason = "…")]` — that attribute is the house convention (see the tests in `cymatics/mod.rs`). This plan's test blocks use only `assert!` / `assert_eq!` / destructuring, so they need no such attribute; if you add an `.expect()` to a test, add the module attribute yourself. Bare `panic!` stays denied.
- **No `assert_eq!(x.is_some(), true)`** → `clippy::bool_assert_comparison`. Use `assert!(x.is_some())`.
- **No `0..(N + 1)`** → `clippy::range_plus_one`. Use `0..=N`.
- **No `u._pad`-style underscore-binding reads** → `clippy::used_underscore_binding`.
- **No deprecated APIs.** `-D warnings` escalates the `deprecated` lint, so a deprecated call fails CI. (This bites Task 5: `egui::Context::screen_rect()` is deprecated in egui 0.34 — use `content_rect()`.)

**The doc gate has no `--all-features` and denies public→private intra-doc links.** CI runs exactly `cargo doc --no-deps --workspace --document-private-items` with `RUSTDOCFLAGS="-D warnings"`. Do not add `--all-features` when reproducing it. A **public** item's rustdoc `[link]` to a `pub(crate)`/private item trips `rustdoc::private_intra_doc_links` (denied) — demote to a plain code span. (Private items *calling* private items in their bodies is fine; only rustdoc `[links]` are policed.) All public items in this plan link only to other public items.

**Full CI gate (run before claiming a task done):**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Per-task steps run a **scoped** subset (`-p wc-core` / `-p wc-sketches`) to stay fast; the controller runs the full workspace gate between tasks.

**There are no GPU tests in CI.** Everything in `crates/wc-core/tests/ui_blur.rs` is `#[ignore]`d (winit needs the macOS main thread), and `cargo xtask capture` returns all-black frames when the window is backgrounded. **Any visually-verifiable change gets an explicit human `cargo rund` step; no automated gate covers rendering.** Design regression tests as GPU-free unit tests over extracted pure functions (this plan extracts `debounce_step`, `fade_duration`/`fades_audio`, and `resize_reload_should_fire`; the panel fix rides the already-pure, already-tested `dock_rect`).

**Commits:** `git add <named paths>` only — **NEVER `git add -A`**. Commit with **`git commit -F <file>`**, **NEVER `git commit -m`** (backticks in the message are command-substituted by zsh). Each commit step below gives the exact message to write to a scratch file first. After committing, `git show --stat HEAD` to confirm only the intended paths are staged. The controller owns branch selection (Plan 01 merged to `v5-alpha`); do not create or switch branches.

**Do not** put `bevy/dynamic_linking` in any manifest `[features]` table. Use `cargo rund` for manual smoke tests.

---

### Task 1: Debounced `WindowResizeSettled` signal

Create the `wc-core` module that watches `WindowResized` + `WindowScaleFactorChanged` and emits `WindowResizeSettled` 250 ms after the last event, and register it in `LifecyclePlugin`. The timing logic is a pure function so it is unit-tested without a window.

**Files:**
- Create: `crates/wc-core/src/lifecycle/window_resize.rs`
- Modify: `crates/wc-core/src/lifecycle/mod.rs` (add `pub mod window_resize;`, register the message, register the system)
- Test: `#[cfg(test)] mod tests` at the footer of `window_resize.rs` (pure `debounce_step`)

**Interfaces:**
- Consumes: `bevy::window::WindowResized`, `bevy::window::WindowScaleFactorChanged` (both `#[derive(Message)]`, registered by Bevy's `WindowPlugin` in production).
- Produces:
  - `pub struct WindowResizeSettled;` — `#[derive(Message, Debug, Clone)]`
  - `pub const RESIZE_DEBOUNCE: std::time::Duration` (250 ms)
  - `pub fn debounce_window_resize(resized: MessageReader<'_, '_, WindowResized>, scale_changed: MessageReader<'_, '_, WindowScaleFactorChanged>, time: Res<'_, Time>, writer: MessageWriter<'_, WindowResizeSettled>, last_event_at: Local<'_, Option<Duration>>)`
  - private `fn debounce_step(last_event_at: Option<Duration>, got_event: bool, now: Duration) -> DebounceOutcome`
  - private `struct DebounceOutcome { next_last_event_at: Option<Duration>, emit: bool }`

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/lifecycle/window_resize.rs` containing *only* the imports and the test module for now, so it fails to compile against the missing `debounce_step` / `RESIZE_DEBOUNCE`:

```rust
use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed base instant so the tests read as wall-clock arithmetic.
    const T0: Duration = Duration::from_millis(1_000);

    #[test]
    fn idle_with_no_pending_timer_emits_nothing() {
        let out = debounce_step(None, false, T0);
        assert!(out.next_last_event_at.is_none());
        assert!(!out.emit);
    }

    #[test]
    fn an_event_arms_the_timer_without_emitting() {
        let out = debounce_step(None, true, T0);
        assert_eq!(out.next_last_event_at, Some(T0), "the arming frame records `now`");
        assert!(
            !out.emit,
            "the arming frame must not emit; the debounce waits for quiet"
        );
    }

    #[test]
    fn a_second_event_before_the_window_pushes_the_deadline_out() {
        // A fresh event 100 ms after the first (< RESIZE_DEBOUNCE) rearms the
        // timer to the new `now` rather than emitting.
        let later = T0 + Duration::from_millis(100);
        let out = debounce_step(Some(T0), true, later);
        assert_eq!(out.next_last_event_at, Some(later), "a fresh event rearms to `now`");
        assert!(!out.emit);
    }

    #[test]
    fn no_emit_one_millisecond_before_the_window_closes() {
        let now = T0 + RESIZE_DEBOUNCE - Duration::from_millis(1);
        let out = debounce_step(Some(T0), false, now);
        assert!(!out.emit, "must not fire before the full quiet window elapses");
        assert_eq!(out.next_last_event_at, Some(T0), "timer stays armed");
    }

    #[test]
    fn emits_and_disarms_exactly_at_the_window() {
        let now = T0 + RESIZE_DEBOUNCE;
        let out = debounce_step(Some(T0), false, now);
        assert!(out.emit, "settle fires once the debounce window elapses");
        assert!(
            out.next_last_event_at.is_none(),
            "and disarms so it fires exactly once per quiet period"
        );
    }

    #[test]
    fn does_not_re_emit_after_disarming() {
        // After a settle `last_event_at` is None; further quiet frames stay silent.
        let out = debounce_step(None, false, T0 + Duration::from_secs(10));
        assert!(!out.emit);
        assert!(out.next_last_event_at.is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

First register the module. In `crates/wc-core/src/lifecycle/mod.rs`, add after `pub mod thermal;` (currently the last `pub mod` line, at `:26`):

```rust
pub mod window_resize;
```

Run: `cargo test -p wc-core --lib lifecycle::window_resize 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function debounce_step in this scope` and `cannot find value RESIZE_DEBOUNCE in this scope`.

- [ ] **Step 3: Write the implementation**

Replace the `use std::time::Duration;` line at the top of `crates/wc-core/src/lifecycle/window_resize.rs` with the module doc, imports, and implementation (leave the `#[cfg(test)] mod tests` block from Step 1 untouched at the footer):

```rust
//! Debounced window-resize settling signal.
//!
//! ## Why this exists
//!
//! Nothing in Line, Dots, or Flame reacts to a window resize: each derives its
//! particle count — and Cymatics its sim-grid resolution — from the window size
//! exactly once, at spawn (`OnEnter`). Pressing F11 fullscreens the window, but
//! the sketch keeps drawing its field into the old extent until the operator
//! navigates away and back, which respawns it. See
//! `docs/superpowers/specs/2026-07-08-windows-remediation-design.md` §2.3.
//!
//! ## What this module does
//!
//! [`debounce_window_resize`] watches both [`bevy::window::WindowResized`] and
//! [`bevy::window::WindowScaleFactorChanged`] and, once [`RESIZE_DEBOUNCE`]
//! (250 ms) has passed with no further event, emits a single
//! [`WindowResizeSettled`] message. Debouncing prevents respawn thrash while a
//! window edge is dragged; in kiosk use a resize only happens at F11, at a
//! monitor re-enumeration, and at the startup scale-factor settle, so the signal
//! fires rarely.
//!
//! Each sketch listens for [`WindowResizeSettled`] and re-runs its spawn path
//! via the shared reload overlay (see
//! [`crate::sketch::reload_on_resize_settled`]).
//!
//! ## Why the timing is a free function
//!
//! [`debounce_step`] is a pure function of `(last_event_at, got_event, now)`, so
//! the settle timing is unit-tested in a tight loop without a window, an egui
//! context, or a GPU — none of which CI has (there are no GPU tests in CI, and
//! the capture harness returns black frames for a backgrounded window). The
//! Bevy system is a thin shell that drains the two message readers and calls it.

use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{WindowResized, WindowScaleFactorChanged};

/// Quiet window that must elapse after the last resize / scale-factor event
/// before [`WindowResizeSettled`] fires.
///
/// 250 ms is short enough that an F11 fullscreen feels immediate, long enough
/// that dragging a window edge (a stream of `WindowResized` events) collapses to
/// a single respawn at the end of the drag rather than one per frame.
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Emitted once the window has stopped resizing for [`RESIZE_DEBOUNCE`].
///
/// Consumed by each sketch's [`crate::sketch::reload_on_resize_settled`]
/// listener, which re-runs the sketch's spawn path so its window-size-derived
/// resources (particle counts, the Cymatics sim grid) are rebuilt at the new
/// extent.
#[derive(Message, Debug, Clone)]
pub struct WindowResizeSettled;

/// Debounce [`WindowResized`] and [`WindowScaleFactorChanged`] into a single
/// [`WindowResizeSettled`] message emitted [`RESIZE_DEBOUNCE`] after the last
/// event.
///
/// Registered unconditionally in [`crate::lifecycle::LifecyclePlugin`] `Update`.
/// Like `drive_reload_state` and the `restart_on_settings_change` listeners,
/// this is a sanctioned always-on message listener: it must observe resize
/// events in every state (including `Home`), and it no-ops in one cheap branch
/// on any frame with no event, so it does not violate "zero systems when idle"
/// (see AGENTS.md, which names this exception class).
///
/// The per-system [`Local`] holds the timestamp of the last observed event
/// (`None` once a settle has been emitted). All timing decisions are delegated
/// to the pure [`debounce_step`].
pub fn debounce_window_resize(
    mut resized: MessageReader<'_, '_, WindowResized>,
    mut scale_changed: MessageReader<'_, '_, WindowScaleFactorChanged>,
    time: Res<'_, Time>,
    mut writer: MessageWriter<'_, WindowResizeSettled>,
    mut last_event_at: Local<'_, Option<Duration>>,
) {
    // Drain BOTH readers every frame. `||` would short-circuit and leave the
    // second reader's messages unread — they would persist and re-trigger next
    // frame — so read each into a bool first, then combine.
    let got_resize = resized.read().count() > 0;
    let got_scale = scale_changed.read().count() > 0;

    let outcome = debounce_step(*last_event_at, got_resize || got_scale, time.elapsed());
    *last_event_at = outcome.next_last_event_at;
    if outcome.emit {
        writer.write(WindowResizeSettled);
        tracing::debug!("window resize settled (debounced); emitting WindowResizeSettled");
    }
}

/// Outcome of one [`debounce_step`]: the timer state to carry to the next frame,
/// and whether a settle should be emitted this frame.
struct DebounceOutcome {
    /// New value for the caller's `last_event_at` timer. `None` means disarmed
    /// (either never armed, or just emitted).
    next_last_event_at: Option<Duration>,
    /// Whether [`WindowResizeSettled`] should be written this frame.
    emit: bool,
}

/// Pure debounce decision.
///
/// Given the previously stored event timestamp, whether an event arrived this
/// frame, and the current elapsed time, returns the next timer state and whether
/// to emit. An event (re)arms the timer to `now`; a settle fires the first frame
/// on which `now` is at least [`RESIZE_DEBOUNCE`] past the armed timestamp, and
/// disarms so it fires exactly once per quiet period.
fn debounce_step(
    last_event_at: Option<Duration>,
    got_event: bool,
    now: Duration,
) -> DebounceOutcome {
    // An event this frame rearms the timer to `now`, pushing the deadline out.
    let armed = if got_event { Some(now) } else { last_event_at };
    match armed {
        Some(t) if now.saturating_sub(t) >= RESIZE_DEBOUNCE => DebounceOutcome {
            next_last_event_at: None,
            emit: true,
        },
        other => DebounceOutcome {
            next_last_event_at: other,
            emit: false,
        },
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib lifecycle::window_resize`

Expected: PASS, 6 tests.

- [ ] **Step 5: Register the message and the debounce system in `LifecyclePlugin`**

In `crates/wc-core/src/lifecycle/mod.rs`, inside `LifecyclePlugin::build`, add the message registration to the builder chain. The chain currently ends (around `:63`) with:

```rust
            .add_message::<crate::input::state::HandTrackingFrame>()
            // Systems
            .add_systems(
```

Insert the new registration between the `HandTrackingFrame` line and the `// Systems` comment:

```rust
            .add_message::<crate::input::state::HandTrackingFrame>()
            // Plan 02: debounced window-resize settling signal (see
            // `window_resize`). Registered here so it exists even for lifecycle
            // tests that do not add a sketch plugin.
            .add_message::<window_resize::WindowResizeSettled>()
            // Systems
            .add_systems(
```

Then register the debounce system. The first `.add_systems(Update, (…).chain());` block closes at `:86` with `            );`, and the next statement (`:88`) is `// Adaptive thermal signal`. Insert between them (match on the existing `// Adaptive thermal signal (Plan 11.8, Seam 1). Spawns the background` comment so the insert lands in the right place; do not duplicate that comment):

```rust
            );

        // Plan 02: debounce `WindowResized` / `WindowScaleFactorChanged` into a
        // single `WindowResizeSettled` (250 ms after the last event). Always-on
        // message listener — it must observe resize events in every state
        // (including `Home`) and no-ops cheaply on any frame with no event, the
        // same always-on category as `reload::drive_reload_state`.
        app.add_systems(Update, window_resize::debounce_window_resize);

        // Adaptive thermal signal (Plan 11.8, Seam 1). Spawns the background
```

- [ ] **Step 6: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib lifecycle::window_resize
```

Expected: clippy clean; 6 tests pass.

Write this message to a scratch file (e.g. via the Write tool to a temp path outside the repo), then commit:

```
feat(lifecycle): debounced WindowResizeSettled signal

Watches WindowResized AND WindowScaleFactorChanged and emits a single
WindowResizeSettled 250 ms after the last event. The timing lives in a
pure debounce_step function unit-tested without a window; the Bevy system
is a thin drain-and-call shell registered always-on in LifecyclePlugin
(it must see resize events in every state, and no-ops when none arrived).

Sketch listeners land in later tasks.
```

```bash
git add crates/wc-core/src/lifecycle/window_resize.rs crates/wc-core/src/lifecycle/mod.rs
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

### Task 2: Give the reload overlay a `ReloadReason` (silent, instant resize reload)

Today the reload overlay always fades to black over 200 ms **and** dips master volume to silence, hops one frame through `Home`, then fades back over 200 ms (`reload.rs:52-53`, `:118-132`, `:139-198`). Applying that to every settled resize would cost F11 a 400 ms blackout with an audio dropout; once Plan 03 ships `start_fullscreen`, every kiosk boot would do it; and a TV waking from sleep would cut the sound — the opposite of what Plan 03 is for. Add a `ReloadReason` so a resize is instant and silent while a settings restart is unchanged.

**Files:**
- Modify: `crates/wc-core/src/lifecycle/reload.rs` (add `ReloadReason` + pure `fade_duration`/`fades_audio`, a `reason` field, the `begin_fade_out` param, the zero-duration `overlay_alpha` guard, and the `drive_reload_state` gating; update the module's own test caller)
- Modify: `crates/wc-core/src/sketch/lifecycle.rs` (import `ReloadReason`; pass `ReloadReason::SettingsRestart` from `restart_on_settings_change`, `:151`)
- Test: new cases in `reload.rs`'s existing `#[cfg(test)] mod tests` (the two pure mappings + a zero-duration terminal-alpha no-NaN case)

**Interfaces:**
- Consumes: nothing from earlier tasks. Existing: `FADE_DURATION` (`:53`), `SketchReloadState` (`:73`), `ReloadPhase` (`:56`).
- Produces:
  - `pub enum ReloadReason { SettingsRestart, WindowResize }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]`, `#[default] SettingsRestart`
  - `SketchReloadState` gains `pub reason: ReloadReason`
  - `SketchReloadState::begin_fade_out(&mut self, now: Duration, pre_fade_volume: f32, return_state: AppState, reason: ReloadReason)` — **signature change** (adds `reason`)
  - private `fn fade_duration(reason: ReloadReason) -> Duration`
  - private `fn fades_audio(reason: ReloadReason) -> bool`

> **Load-bearing detail (call it out):** a `WindowResize` reason gives a zero-length fade. `overlay_alpha` divides elapsed by `fade_secs`; at `elapsed == 0` that is `0.0 / 0.0 == NaN`, and `NaN.clamp(0.0, 1.0) == NaN`, which would paint a garbage overlay. The zero-duration guard in `overlay_alpha` (return the terminal alpha directly) is what prevents that. On the phase-advance side, `elapsed >= Duration::ZERO` is always true, so `drive_reload_state` advances a zero-length leg on the **first** frame instead of ever evaluating the divide against a running timer — one black repaint frame, then done.

- [ ] **Step 1: Write the failing tests**

Add these three tests inside the existing `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/lifecycle/reload.rs` (alongside the current `idle_alpha_is_zero` etc.):

```rust
    #[test]
    fn settings_restart_fades_over_the_full_duration_and_dips_audio() {
        assert_eq!(fade_duration(ReloadReason::SettingsRestart), FADE_DURATION);
        assert!(fades_audio(ReloadReason::SettingsRestart));
    }

    #[test]
    fn window_resize_is_instant_and_silent() {
        assert_eq!(fade_duration(ReloadReason::WindowResize), Duration::ZERO);
        assert!(!fades_audio(ReloadReason::WindowResize));
    }

    #[test]
    fn zero_duration_reload_gives_terminal_alpha_without_nan() {
        // A WindowResize reason has a zero-length fade. `overlay_alpha` must
        // return the terminal opacity (opaque in FadeOut, transparent in
        // FadeIn) rather than NaN from a divide-by-zero.
        let fade_out = SketchReloadState {
            phase: ReloadPhase::FadeOut,
            reason: ReloadReason::WindowResize,
            ..Default::default()
        };
        let a = fade_out.overlay_alpha(Duration::ZERO);
        assert!(a.is_finite(), "alpha must not be NaN");
        assert!(a > 0.99, "FadeOut with zero duration is fully opaque, got {a}");

        let fade_in = SketchReloadState {
            phase: ReloadPhase::FadeIn,
            reason: ReloadReason::WindowResize,
            started_at: Duration::ZERO,
            ..Default::default()
        };
        let b = fade_in.overlay_alpha(Duration::ZERO);
        assert!(b.is_finite(), "alpha must not be NaN");
        assert!(b < 0.01, "FadeIn with zero duration is fully transparent, got {b}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib lifecycle::reload 2>&1 | head -25`

Expected: FAIL to compile — `cannot find function fade_duration`, `cannot find function fades_audio`, `cannot find type/variant ReloadReason`, and `SketchReloadState` has no field `reason`.

- [ ] **Step 3: Add `ReloadReason` and the two pure mappings**

In `crates/wc-core/src/lifecycle/reload.rs`, immediately after the `FADE_DURATION` const (`:53`) and before `pub enum ReloadPhase` (`:56`), insert:

```rust
/// Why a reload was requested. Selects the fade profile.
///
/// A settings restart fades to black and dips audio over [`FADE_DURATION`]; a
/// window resize is silent and instant (one black repaint frame nobody sees, no
/// audio command), because the reload exists only to re-run the sketch's spawn
/// path at the new extent — there is nothing to fade, and a kiosk waking from
/// sleep must not have its sound cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadReason {
    /// A `requires_restart` settings change. The historical behaviour: 200 ms
    /// fade out, audio dip to silence, one `Home` frame, 200 ms fade in.
    #[default]
    SettingsRestart,
    /// A settled window resize (F11, monitor re-enumeration, startup scale
    /// settle). Instant and silent.
    WindowResize,
}

/// Fade-leg duration for a reload `reason`: [`FADE_DURATION`] for a settings
/// restart, [`Duration::ZERO`] for a window resize (which advances on the first
/// frame). Pure so the mapping is unit-testable without an app.
fn fade_duration(reason: ReloadReason) -> Duration {
    match reason {
        ReloadReason::SettingsRestart => FADE_DURATION,
        ReloadReason::WindowResize => Duration::ZERO,
    }
}

/// Whether a reload `reason` fades the master volume. A window resize must not
/// touch audio (a kiosk waking from sleep would otherwise cut its sound). Pure
/// so the mapping is unit-testable without an app.
fn fades_audio(reason: ReloadReason) -> bool {
    match reason {
        ReloadReason::SettingsRestart => true,
        ReloadReason::WindowResize => false,
    }
}
```

- [ ] **Step 4: Add the `reason` field and thread it through `begin_fade_out`**

In `SketchReloadState` (`:73-87`), add a field after `pub return_state: AppState,`:

```rust
    /// Why this reload was requested; selects the fade profile (see
    /// [`ReloadReason`]). Defaults to [`ReloadReason::SettingsRestart`], the
    /// historical behaviour.
    pub reason: ReloadReason,
```

Replace `begin_fade_out` (`:106-111`) with the reason-carrying version:

```rust
    /// Start the `FadeOut` phase. Stores the current master volume, the sketch
    /// state to return to, and the [`ReloadReason`] that selects the fade
    /// profile (a settings restart fades + dips audio; a window resize is
    /// instant and silent).
    ///
    /// `return_state` must be the currently active sketch state (e.g.
    /// `AppState::Line`) so [`drive_reload_state`] navigates back correctly.
    pub fn begin_fade_out(
        &mut self,
        now: Duration,
        pre_fade_volume: f32,
        return_state: AppState,
        reason: ReloadReason,
    ) {
        self.phase = ReloadPhase::FadeOut;
        self.started_at = now;
        self.pre_fade_volume = pre_fade_volume;
        self.return_state = return_state;
        self.reason = reason;
    }
```

- [ ] **Step 5: Guard `overlay_alpha` against the zero-length fade**

Replace `overlay_alpha` (`:118-132`) with the reason-aware, NaN-guarded version:

```rust
    /// Compute the current overlay alpha (0.0 = transparent, 1.0 = opaque).
    ///
    /// Returns 0.0 during `Idle`; ramps 0→1 during `FadeOut`; holds at 1.0
    /// during `Switch`; ramps 1→0 during `FadeIn`. The ramp duration is
    /// [`fade_duration`] of [`Self::reason`]; a zero-length fade (a window
    /// resize) returns the terminal alpha directly, avoiding a `0.0 / 0.0` NaN.
    #[must_use]
    pub fn overlay_alpha(&self, now: Duration) -> f32 {
        let fade_secs = fade_duration(self.reason).as_secs_f32();
        match self.phase {
            ReloadPhase::Idle => 0.0,
            ReloadPhase::FadeOut => {
                // Zero-duration (WindowResize): fully opaque at once. Guard the
                // divide — `elapsed / 0.0` is NaN at elapsed 0 and NaN.clamp is
                // NaN. `drive_reload_state` advances the phase on the first
                // frame, so this is a single black repaint frame nobody sees.
                if fade_secs <= 0.0 {
                    return 1.0;
                }
                let t = now.saturating_sub(self.started_at).as_secs_f32() / fade_secs;
                t.clamp(0.0, 1.0)
            }
            ReloadPhase::Switch => 1.0,
            ReloadPhase::FadeIn => {
                // Zero-duration: already fully transparent (same NaN guard).
                if fade_secs <= 0.0 {
                    return 0.0;
                }
                let t = now.saturating_sub(self.started_at).as_secs_f32() / fade_secs;
                (1.0 - t).clamp(0.0, 1.0)
            }
        }
    }
```

- [ ] **Step 6: Gate the audio pushes and the leg durations in `drive_reload_state`**

Replace the body of `drive_reload_state` (`:139-198`) with the reason-aware version. It captures `reason`/`fade` once, pushes volume only when `fades_audio(reason)`, and compares the phase timers against `fade` (which is `Duration::ZERO` for a resize, so those legs advance on the first frame):

```rust
pub fn drive_reload_state(
    mut state: ResMut<'_, SketchReloadState>,
    time: Res<'_, Time>,
    mut next_app: ResMut<'_, NextState<super::state::AppState>>,
    mut audio_cmd: Option<
        bevy::ecs::system::NonSendMut<'_, crate::audio::ring::AudioCommandSender>,
    >,
) {
    let now = time.elapsed();
    let reason = state.reason;
    // Per-leg duration for this reason: FADE_DURATION for a settings restart,
    // ZERO for a window resize (which then advances on the first frame below).
    let fade = fade_duration(reason);
    let alpha = state.overlay_alpha(now);

    // Push the audio fade only for reasons that fade audio (a window resize does
    // not touch the master volume). Volume is the inverse of the overlay alpha:
    // full when transparent, silent when fully opaque.
    if fades_audio(reason) && matches!(state.phase, ReloadPhase::FadeOut | ReloadPhase::FadeIn) {
        if let Some(ref mut sender) = audio_cmd {
            let _ = sender.push(crate::audio::command::AudioCommand::SetMasterVolume(
                1.0 - alpha,
            ));
        }
    }

    match state.phase {
        ReloadPhase::Idle => {
            // Nothing to drive.
        }
        ReloadPhase::FadeOut => {
            if now.saturating_sub(state.started_at) >= fade {
                // FadeOut complete — switch to Home so the sketch exits cleanly.
                next_app.set(super::state::AppState::Home);
                state.phase = ReloadPhase::Switch;
                tracing::debug!("reload overlay: FadeOut complete → Switch (Home)");
            }
        }
        ReloadPhase::Switch => {
            // One frame in Home; arm the re-entry into the sketch that
            // triggered the reload (set by `begin_fade_out`).
            let return_to = state.return_state;
            next_app.set(return_to);
            state.phase = ReloadPhase::FadeIn;
            state.started_at = now;
            tracing::debug!("reload overlay: Switch → FadeIn ({:?})", return_to);
        }
        ReloadPhase::FadeIn => {
            if now.saturating_sub(state.started_at) >= fade {
                // FadeIn complete — restore volume and return to Idle.
                let restore_vol = state.pre_fade_volume;
                state.phase = ReloadPhase::Idle;
                // Restore volume only for reasons that dipped it; a window
                // resize never issued a volume command, so it issues none here.
                if fades_audio(reason) {
                    if let Some(ref mut sender) = audio_cmd {
                        let _ = sender.push(crate::audio::command::AudioCommand::SetMasterVolume(
                            restore_vol,
                        ));
                    }
                }
                tracing::debug!("reload overlay: FadeIn complete → Idle");
            }
        }
    }
}
```

- [ ] **Step 7: Update the existing `begin_fade_out` callers**

Two callers pass the old argument list; both must add the reason.

In `crates/wc-core/src/lifecycle/reload.rs`, the module test at `:218`:

```rust
        s.begin_fade_out(Duration::ZERO, 1.0, AppState::Line);
```

becomes:

```rust
        s.begin_fade_out(Duration::ZERO, 1.0, AppState::Line, ReloadReason::SettingsRestart);
```

In `crates/wc-core/src/sketch/lifecycle.rs`, first extend the reload import (`:49`):

```rust
use crate::lifecycle::reload::{ReloadReason, SketchReloadState};
```

Then the `restart_on_settings_change` call (`:151`):

```rust
            reload_state.begin_fade_out(time.elapsed(), pre_fade_volume, S::STATE);
```

becomes:

```rust
            reload_state.begin_fade_out(
                time.elapsed(),
                pre_fade_volume,
                S::STATE,
                ReloadReason::SettingsRestart,
            );
```

- [ ] **Step 8: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib lifecycle::reload
```

Expected: clippy clean; the new three tests plus the existing reload tests pass. The existing tests use `SketchReloadState::default()` (reason `SettingsRestart`), so their 200 ms-fade assertions are unchanged.

Write this message to a scratch file, then commit:

```
feat(reload): ReloadReason — silent, instant window-resize reloads

The reload overlay always faded to black over 200 ms and dipped master
volume to silence. Adding ReloadReason::WindowResize gives a zero-length,
audio-free reload (one black repaint frame, no SetMasterVolume) so a
resize respawn does not blackout a kiosk boot or cut a TV's audio on
wake, while ReloadReason::SettingsRestart keeps the existing fade.

fade_duration and fades_audio are pure and unit-tested; overlay_alpha
guards the zero-length divide (0/0 -> NaN) by returning the terminal
alpha, and drive_reload_state advances a zero-length leg on the first
frame. begin_fade_out gains the reason; the settings-restart caller
passes SettingsRestart.
```

```bash
git add crates/wc-core/src/lifecycle/reload.rs crates/wc-core/src/sketch/lifecycle.rs
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

### Task 3: `reload_on_resize_settled::<S>` — respawn a sketch at the new size

Add a generic listener that, when a resize settles while its sketch is the running one, drives the reload overlay (with `ReloadReason::WindowResize`) to re-run `OnEnter` at the new size. It mirrors `restart_on_settings_change`: **registered always-on**, gating internally.

**Files:**
- Modify: `crates/wc-core/src/sketch/lifecycle.rs` (add the `WindowResizeSettled` import, the generic system, the pure predicate, and a `#[cfg(test)] mod tests`)
- Modify: `crates/wc-core/src/sketch/mod.rs:26-29` (re-export the new function)
- Test: `#[cfg(test)] mod tests` at the footer of `lifecycle.rs` (pure `resize_reload_should_fire`)

**Interfaces:**
- Consumes: `WindowResizeSettled` (Task 1) from `crate::lifecycle::window_resize`; `ReloadReason` (Task 2, import already added in Task 2). In-scope: `SketchLifecycle` (`:88`), `SketchReloadState` / `ReloadReason` (`:49` import), `AppState` (`:50` import), `AudioState` (`:48` import).
- Produces:
  - `pub fn reload_on_resize_settled<S: SketchLifecycle>(settled: MessageReader<'_, '_, WindowResizeSettled>, time: Res<'_, Time>, current: Res<'_, State<AppState>>, reload_state: ResMut<'_, SketchReloadState>, audio_state: Option<Res<'_, AudioState>>)`
  - private `fn resize_reload_should_fire(got_settle: bool, in_state: bool, reload_idle: bool) -> bool`

**Why always-on, not `.run_if(sketch_active(…))`.** `sketch_active` is `AppState == target && SketchActivity == Active` (`scheduling.rs:24`), so gating on it would make the listener dead during `Idle` and `Screensaver` — exactly the unattended-kiosk state. A TV that sleeps and wakes overnight re-enumerates the monitor and fires a resize; the sketch must respawn at the new size then too, not only after someone touches it (that is the failure Plan 03 exists to prevent). So this mirrors `restart_on_settings_change`, which is registered with **no** `run_if` and gates internally on `**current == S::STATE && reload_state.is_idle()`.

**Consequence, documented not solved.** The reload's `Home` hop destroys `SketchActivity` (a `#[source]` sub-state of the sketch `AppState`s), so re-entering the sketch resets it to the default `Active`. A resize that arrives during attract mode therefore returns the sketch to `Active`, and the idle timer re-engages and re-enters `Idle`/`Screensaver` after its normal timeout. For an unattended kiosk that is acceptable — a correct-size sketch that re-idles beats a wrong-size attract field.

- [ ] **Step 1: Write the failing test**

Add to the footer of `crates/wc-core/src/sketch/lifecycle.rs` (there is currently no `mod tests` block in this file — add one at the very end):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The reload only begins on a real settle while this is the running sketch
    /// and no reload is already in flight. The negatives are the ones that
    /// matter: no stray fire, no firing for another sketch, and no re-trigger
    /// mid-reload (the reload drives its own `Sketch → Home → Sketch` hop, which
    /// would otherwise loop).
    #[test]
    fn fires_only_on_a_settle_in_this_sketch_while_reload_is_idle() {
        assert!(
            resize_reload_should_fire(true, true, true),
            "settle, our sketch, idle → fire"
        );
        assert!(
            !resize_reload_should_fire(false, true, true),
            "no settle → nothing"
        );
        assert!(
            !resize_reload_should_fire(true, false, true),
            "settle but not our sketch → nothing"
        );
        assert!(
            !resize_reload_should_fire(true, true, false),
            "settle mid-reload must not re-trigger (its own Home hop would loop)"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib sketch::lifecycle 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function resize_reload_should_fire in this scope`.

- [ ] **Step 3: Write the implementation**

Add the import for `WindowResizeSettled` next to the other `use crate::lifecycle::…` lines (the `ReloadReason` import was added in Task 2, so `:49` now reads `use crate::lifecycle::reload::{ReloadReason, SketchReloadState};`):

```rust
use crate::lifecycle::window_resize::WindowResizeSettled;
```

Insert the new public function and its private predicate immediately **after** `restart_on_settings_change` (its closing brace is at `:159`) and **before** the `apply_render_profile` doc (`:161`):

```rust
/// Window-resize listener: re-runs sketch `S`'s spawn path when the window has
/// settled at a new size, by driving the reload overlay with
/// [`ReloadReason::WindowResize`] (instant and silent — no fade, no audio dip).
///
/// Reuse rationale: a sketch derives its window-size-dependent resources
/// (particle counts; the Cymatics sim grid) in its `OnEnter` spawn systems, so
/// the cleanest "respawn at the new size" is to re-run `OnEnter`. The reload
/// overlay already performs exactly that `Sketch -> Home -> Sketch` round-trip
/// (see [`crate::lifecycle::reload`]); the `WindowResize` reason makes it cost a
/// single black repaint frame with no audio dropout. Rebuilding rather than
/// rescaling in place is required because a sketch's element *count* changes
/// with size (Dots' grid, Line's particle count), so the GPU buffers must be
/// reallocated.
///
/// Registered **always-on** (no `run_if`), mirroring `restart_on_settings_change`
/// and gating internally on being the running sketch with no reload in flight.
/// It deliberately does **not** gate on `sketch_active`: a resize during `Idle`
/// or the attract screensaver (e.g. a TV re-enumerating after sleep) must still
/// respawn the sketch at the new size. The `Home` hop resets the sketch's
/// `SketchActivity` sub-state to `Active`, after which the idle timer re-engages
/// normally — acceptable for an unattended kiosk. It is a sanctioned always-on
/// listener (see AGENTS.md); it no-ops in one cheap branch when no settle
/// arrived.
///
/// [`resize_reload_should_fire`] encodes the gate.
pub fn reload_on_resize_settled<S: SketchLifecycle>(
    mut settled: MessageReader<'_, '_, WindowResizeSettled>,
    time: Res<'_, Time>,
    current: Res<'_, State<AppState>>,
    mut reload_state: ResMut<'_, SketchReloadState>,
    // Absent in headless (MinimalPlugins) test harnesses and before the cpal
    // stream is up; fall back to full volume then. (A resize never dips audio,
    // so this value is only carried for symmetry with the settings-restart path.)
    audio_state: Option<Res<'_, AudioState>>,
) {
    // Drain the reader every frame regardless of the decision.
    let got_settle = settled.read().count() > 0;
    if !resize_reload_should_fire(got_settle, **current == S::STATE, reload_state.is_idle()) {
        return;
    }
    let pre_fade_volume = audio_state.as_ref().map_or(1.0, |s| s.volume);
    reload_state.begin_fade_out(
        time.elapsed(),
        pre_fade_volume,
        S::STATE,
        ReloadReason::WindowResize,
    );
    tracing::debug!(
        "window resize settled while in '{}' — beginning a silent, instant reload at the new size",
        S::STORAGE_KEY
    );
}

/// Whether a settled resize should begin a reload this frame.
///
/// Pure so the gate is unit-testable without an app: fire only when a settle
/// arrived AND this is the running sketch AND no reload is already in progress
/// (a reload drives its own `Sketch -> Home -> Sketch` transition, which must
/// not re-trigger one).
fn resize_reload_should_fire(got_settle: bool, in_state: bool, reload_idle: bool) -> bool {
    got_settle && in_state && reload_idle
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib sketch::lifecycle`

Expected: PASS (`fires_only_on_a_settle_in_this_sketch_while_reload_is_idle`).

- [ ] **Step 5: Re-export the function**

In `crates/wc-core/src/sketch/mod.rs`, extend the `pub use lifecycle::{…}` block (`:26-29`):

```rust
pub use lifecycle::{
    apply_render_profile, reload_on_resize_settled, reset_render_profile,
    restart_on_settings_change, RenderProfile, SketchLifecycle, RESTART_DEBOUNCE,
};
```

- [ ] **Step 6: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib sketch::lifecycle
cargo doc --no-deps -p wc-core --document-private-items
```

Expected: clippy clean; tests pass; doc builds (all links from `reload_on_resize_settled` are to public items — `ReloadReason`, `WindowResizeSettled`, `crate::lifecycle::reload`).

Write this message to a scratch file, then commit:

```
feat(sketch): reload_on_resize_settled — respawn a sketch at the new size

A generic always-on listener that, on WindowResizeSettled while its sketch
is running and no reload is in flight, drives the reload overlay with
ReloadReason::WindowResize to re-run OnEnter at the new size. Registered
always-on (not gated on sketch_active) so a resize during idle/screensaver
— a TV re-enumerating after sleep — still respawns; the Home hop resets
SketchActivity to Active and the idle timer re-engages. Fire decision is a
pure predicate, unit-tested. No spawn system is edited.
```

```bash
git add crates/wc-core/src/sketch/lifecycle.rs crates/wc-core/src/sketch/mod.rs
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

### Task 4: Register the listener in Line, Dots, Flame, and Cymatics

Wire `reload_on_resize_settled::<S>` into each sketch plugin, **always-on (no `run_if`)**, mirroring the `restart_on_settings_change::<S>` registration right above it. Each also defensively registers the message so a test that builds only the sketch plugin (without `LifecyclePlugin`) still has it (Bevy dedups; `LifecyclePlugin` is the canonical registrant, Task 1).

**Files:**
- Modify: `crates/wc-sketches/src/line/mod.rs` (after the restart registration at `:235-238`)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (after the restart registration at `:218-221`)
- Modify: `crates/wc-sketches/src/flame/mod.rs` (after the restart registration at `:191-194`)
- Modify: `crates/wc-sketches/src/cymatics/mod.rs` (after the restart registration at `:162-165`)

**Interfaces:**
- Consumes: `wc_core::sketch::reload_on_resize_settled::<S>` (Task 3) and `wc_core::lifecycle::window_resize::WindowResizeSettled` (Task 1). All four settings structs impl `SketchLifecycle` (`line/settings.rs`, `dots/settings.rs`, `flame/settings.rs:424`, `cymatics/settings.rs:566`), so each monomorphization type-checks.
- Produces: nothing consumed later.

No new automated test is added here — the machinery is unit-tested in Tasks 1–3 (debounce timing, reason mapping / NaN guard, fire predicate) and in `reload.rs` (the fade state machine). The end-to-end path (message → reload → respawn → new extent) is window/GPU-dependent and is covered by the human verification in Step 6.

- [ ] **Step 1: Line**

In `crates/wc-sketches/src/line/mod.rs`, the restart listener is registered at `:235-238`:

```rust
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::LineSettings>,
        );
```

Immediately after that call (before the `}` closing `build`), insert:

```rust

        // Plan 02: re-run the spawn path at the new window size when a resize
        // settles, via a silent/instant reload. Registered ALWAYS-ON (no
        // run_if), mirroring `restart_on_settings_change` above — a resize
        // during idle/screensaver (a display re-enumerating after sleep) must
        // still respawn; the listener gates internally. Defensive `add_message`
        // so a test that builds this plugin without wc-core's LifecyclePlugin
        // still has the message (Bevy dedups; LifecyclePlugin is canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::LineSettings>,
        );
```

- [ ] **Step 2: Dots**

In `crates/wc-sketches/src/dots/mod.rs`, the restart listener is registered at `:218-221`:

```rust
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::DotsSettings>,
        );
```

Immediately after that call, insert:

```rust

        // Plan 02: re-run the spawn path at the new window size when a resize
        // settles, via a silent/instant reload. Registered ALWAYS-ON (no
        // run_if), mirroring `restart_on_settings_change` above — a resize
        // during idle/screensaver (a display re-enumerating after sleep) must
        // still respawn; the listener gates internally. Defensive `add_message`
        // so a test that builds this plugin without wc-core's LifecyclePlugin
        // still has the message (Bevy dedups; LifecyclePlugin is canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::DotsSettings>,
        );
```

- [ ] **Step 3: Flame**

In `crates/wc-sketches/src/flame/mod.rs`, the restart listener is registered at `:191-194`:

```rust
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<settings::FlameSettings>,
        );
```

Immediately after that call, insert:

```rust

        // Plan 02: re-run the spawn path at the new window size when a resize
        // settles, via a silent/instant reload. Registered ALWAYS-ON (no
        // run_if), mirroring `restart_on_settings_change` above — a resize
        // during idle/screensaver (a display re-enumerating after sleep) must
        // still respawn; the listener gates internally. Defensive `add_message`
        // so a test that builds this plugin without wc-core's LifecyclePlugin
        // still has the message (Bevy dedups; LifecyclePlugin is canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<settings::FlameSettings>,
        );
```

- [ ] **Step 4: Cymatics**

In `crates/wc-sketches/src/cymatics/mod.rs`, the restart listener is registered at `:162-165`:

```rust
        app.add_systems(
            Update,
            wc_core::sketch::restart_on_settings_change::<CymaticsSettings>,
        );
```

Immediately after that call, insert (Cymatics imports `CymaticsSettings` directly, `:58`):

```rust

        // Plan 02: re-run the spawn path at the new window size when a resize
        // settles, via a silent/instant reload. This is also what re-inits the
        // sim grid, which `spawn_cymatics` derives from the window aspect.
        // Registered ALWAYS-ON (no run_if), mirroring `restart_on_settings_change`
        // above — a resize during idle/screensaver (a display re-enumerating
        // after sleep) must still respawn; the listener gates internally.
        // Defensive `add_message` so a test that builds this plugin without
        // wc-core's LifecyclePlugin still has the message (Bevy dedups;
        // LifecyclePlugin is canonical).
        app.add_message::<wc_core::lifecycle::window_resize::WindowResizeSettled>();
        app.add_systems(
            Update,
            wc_core::sketch::reload_on_resize_settled::<CymaticsSettings>,
        );
```

- [ ] **Step 5: Run the scoped gate**

```bash
cargo fmt --all
cargo clippy -p wc-sketches --all-targets --all-features -- -D warnings
cargo nextest run -p wc-sketches
```

Expected: clippy clean; existing `wc-sketches` tests pass. A double-registered system panics at startup, so a green run also confirms each `reload_on_resize_settled::<S>` monomorphization is a distinct system.

- [ ] **Step 6: Human verification — F11 respawn (no automated gate covers this)**

There are no GPU tests in CI and `cargo xtask capture` returns black frames when backgrounded, so a human must run this.

```bash
cargo rund
```

For **each** of Line, Dots, Flame, Cymatics (press z/x to cycle, or use the picker):

1. Enter the sketch. Confirm it fills the (windowed) 1280×720 client area.
2. Press **F11**. Expected: an essentially instant transition (at most a single black repaint frame) with **no audio dropout**, after which the sketch fills the **entire** fullscreen extent — not a framed field drawn into the old 1280×720 rectangle. This is the reported "F11 gives framed fullscreen" bug fixed, and the reload is silent (Task 2).
3. Press **F11** again to return to windowed. Expected: again instant + silent, then the field fills the windowed area correctly.
4. For **Cymatics** specifically, confirm the wave field (not just the quad) covers the whole screen after F11 — i.e. the sim grid re-derived from the new aspect, no letterboxed or stretched sim region.

Also confirm the debounce feels right: a single F11 produces exactly one respawn (not a flurry), and audio keeps playing across the toggle (no dip to silence).

Optional deterministic-harness sanity (foregrounded, not backgrounded): `cargo xtask capture line` and `cargo xtask capture dots`. The capture harness does not resize the window, so `debounce_window_resize` sees no events and `reload_on_resize_settled` never fires — captures must still match their existing baselines. Drift here would mean the always-on debounce system is doing something it should not.

- [ ] **Step 7: Commit**

Write this message to a scratch file, then commit:

```
feat(sketches): respawn Line/Dots/Flame/Cymatics on window-resize settle

Each sketch plugin registers reload_on_resize_settled::<S> always-on
(mirroring restart_on_settings_change), so a settled resize (F11, monitor
re-enumeration, startup scale settle) re-runs the sketch's spawn path at
the new extent via the silent/instant reload. Fixes "F11 gives framed
fullscreen" and re-inits the Cymatics sim grid from the new aspect.
Always-on rather than gated on sketch_active so a display re-enumerating
after sleep still respawns. Each also defensively registers the
WindowResizeSettled message (Bevy dedups). Verified by hand with cargo
rund + F11 on all four (instant, no audio dropout, fills the screen).
```

```bash
git add crates/wc-sketches/src/line/mod.rs crates/wc-sketches/src/dots/mod.rs crates/wc-sketches/src/flame/mod.rs crates/wc-sketches/src/cymatics/mod.rs
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

### Task 5: Fix the settings-dock misplacement (derive geometry from `ctx.content_rect()`)

The dock geometry is computed from Bevy's `Window` (logical pixels) but the `egui::Area` is placed and sized in egui points. They agree in steady state and diverge for the one frame after a scale-factor change, where `bevy_egui`'s screen rect still reflects the previous frame's `pixels_per_point` (an upstream one-frame lag; recorded in the index's Part 4 as a Bevy issue to file). Feeding `dock_rect` the same units egui lays out in keeps the dock anchored inside egui's believed screen during that frame instead of overflowing the right edge and snapping in. The spike is done and root-caused (index Plan 02); this is the small fix. **`dock_rect` is unchanged, so its unit tests stand.**

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user/mod.rs` (delete the `Window` query at `:162-169`; recompute geometry from `ctx.content_rect()` after the ctx clone at `:191-192`)
- Test: none new. The already-pure `dock::dock_rect` keeps its tests at `dock.rs:187-209`; verification is those tests still passing plus a human `cargo rund` check.

**Interfaces:**
- Consumes: `dock::dock_rect(f32, f32) -> (f32, f32, f32, f32)` (unchanged, `dock.rs:112`) and `egui::Context::content_rect() -> egui::Rect` (egui 0.34, `context.rs:2823`). `dock_x/dock_y/dock_w/dock_h` are consumed at `:202`, `:207`, `:208`, all after the new definition site, so ordering is preserved.
- Produces: nothing.

> **Use `content_rect()`, not `screen_rect()`.** `egui::Context::screen_rect()` is **deprecated** in egui 0.34.3 (`context.rs:2842-2847`: *"screen_rect has been split into viewport_rect() and content_rect(). You likely should use content_rect()"*) and would fail the `-D warnings` `deprecated` lint. `content_rect()` is the non-deprecated equivalent (`screen_rect`'s own body is `self.input(|i| i.content_rect()).round_ui()`), and it is the call this codebase already uses to fill the screen (`crates/wc-core/src/ui/reload_overlay.rs`: `let screen = ctx.content_rect();`). Anchor confirmed at `egui-0.34.3/src/context.rs:2823` (`pub fn content_rect(&self) -> Rect`).

- [ ] **Step 1: Delete the `Window`-based geometry read**

In `crates/wc-core/src/settings/panel_user/mod.rs`, replace the block at `:162-169`:

```rust
    // Read window size for the right-dock geometry; fall back to 1280×720.
    let (window_width, window_height) = {
        let mut q =
            world.query_filtered::<&bevy::window::Window, With<bevy::window::PrimaryWindow>>();
        q.single(world)
            .map_or((1280.0, 720.0), |w| (w.width(), w.height()))
    };
    let (dock_x, dock_y, dock_w, dock_h) = dock::dock_rect(window_width, window_height);
```

with a one-line placeholder comment (the geometry now computes after the ctx clone, in Step 2):

```rust
    // Dock geometry is derived from egui's content rect below, after the egui
    // context is cloned (see the note there) — not from Bevy's `Window`.
```

- [ ] **Step 2: Recompute geometry from `ctx.content_rect()` after the ctx clone**

Still in `crates/wc-core/src/settings/panel_user/mod.rs`, the ctx clone is at `:191-192`:

```rust
    let ctx = ctx.clone();
    state.apply(world);
```

Insert the geometry computation immediately after `state.apply(world);` (and before the `#[cfg(feature = "templates")] let template_rows = …` line at `:197`):

```rust
    let ctx = ctx.clone();
    state.apply(world);

    // Right-dock geometry from egui's own content rect (points), NOT Bevy's
    // `Window` (logical pixels). The two agree in steady state but diverge for
    // the one frame after a scale-factor change: `bevy_egui`'s screen rect still
    // reflects the previous frame's `pixels_per_point` (a one-frame upstream
    // lag), while `Window` already reports the new size. Mixing the two placed
    // the dock using new-size pixels inside egui's stale-size layout, so it
    // overflowed the right edge for a frame and then snapped in — exactly the
    // reported "panel loads oversized then corrects itself". Feeding `dock_rect`
    // the same units egui lays out in keeps the dock anchored inside whatever
    // egui currently believes the screen to be. `dock_rect` stays pure; only its
    // input source changed, so its unit tests stand. `content_rect()` is the
    // non-deprecated call (see `ui::reload_overlay`); `screen_rect()` is
    // deprecated in egui 0.34 and would fail `-D warnings`.
    let content = ctx.content_rect();
    let (screen_w, screen_h) = if content.width() > 1.0 && content.height() > 1.0 {
        (content.width(), content.height())
    } else {
        // egui has not reported a real size yet (the very first frames); fall
        // back to the old default so the dock is never wildly misplaced.
        (1280.0, 720.0)
    };
    let (dock_x, dock_y, dock_w, dock_h) = dock::dock_rect(screen_w, screen_h);
```

- [ ] **Step 3: Verify the anchors still resolve and the dock tests stand**

```bash
rg -n "window_width|window_height|PrimaryWindow" crates/wc-core/src/settings/panel_user/mod.rs
```

Expected: no matches — the deleted query's bindings are gone. (`bevy::prelude::*` is a glob import, so dropping the `Window`/`PrimaryWindow` query yields no unused-import warning.)

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib settings::panel_user::dock
```

Expected: clippy clean; the existing `dock_rect_anchors_right_and_clamps_width`, `tab_routing_is_total`, `sketch_keys_route_to_sketch_tab`, and `field_visible_gates_dev_on_advanced` tests pass unchanged. `dock_x/dock_y` still feed `fixed_pos` (`:202`) and `dock_w/dock_h` still feed `set_min_size`/`set_max_size` (`:207-208`), now defined earlier in the function — the clippy run above proves the crate compiles.

- [ ] **Step 4: Human verification — dock placement (no automated gate covers this)**

```bash
cargo rund
```

1. Enter any sketch, press the settings cog (or the key that toggles the dock) to open the settings panel. Expected: it appears docked to the right, inset from the right/bottom/top edges, fully on-screen — not overflowing the right edge or oversized.
2. With the panel **open**, press **F11** to toggle fullscreen (this changes the scale factor and triggered the tester's report). Expected: the dock stays anchored to the right and correctly sized across the transition — no jump off the right edge, no wrong-scale render that snaps back.
3. Toggle F11 a few times with the panel open. The dock should track the right edge each time without a visible misplacement frame.

- [ ] **Step 5: Commit**

Write this message to a scratch file, then commit:

```
fix(settings): dock geometry from egui content_rect, not Bevy Window

draw_user_panel placed the settings dock's egui::Area in points but sized
it from Window logical pixels. The two diverge for the one frame after a
scale-factor change, where bevy_egui's screen rect still uses the previous
frame's pixels_per_point (an upstream one-frame lag). That frame mixed
new-size pixels into egui's stale-size layout, so the dock overflowed the
right edge and snapped back — the reported "panel loads oversized then
corrects itself", triggered together with the F11 bug because F11 changes
the scale factor. Derive geometry from ctx.content_rect() (screen_rect is
deprecated in egui 0.34) so both sides speak points. dock_rect is
unchanged; its unit tests stand.
```

```bash
git add crates/wc-core/src/settings/panel_user/mod.rs
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

### Task 6: Update AGENTS.md's always-on-exception sentence

The always-on listeners this plan adds (`debounce_window_resize`, the four `reload_on_resize_settled::<S>` registrations) are a new sanctioned exception to "zero systems when idle". AGENTS.md currently names only `restart_on_*_settings_change`; leaving it unamended makes the doc contradict the code — the exact class of defect Plan 01's review caught. Update the sentence.

**Files:**
- Modify: `AGENTS.md:53`

**Interfaces:** none.

- [ ] **Step 1: Amend the sentence**

In `AGENTS.md`, the bullet at `:53` currently reads:

```markdown
- Sketches must run zero systems when in `SketchActivity::Idle`. This is enforced by convention and review, not a CI check — the sanctioned always-on exception is the three `restart_on_*_settings_change` systems (settings-reload listeners), which are expected to keep running in `Idle`.
```

Replace it with:

```markdown
- Sketches must run zero systems when in `SketchActivity::Idle`. This is enforced by convention and review, not a CI check. The sanctioned always-on exceptions are the message listeners that must observe their triggering event in every activity and no-op cheaply otherwise: the `restart_on_settings_change` settings-reload listeners; the window-resize debounce `debounce_window_resize` (`lifecycle/window_resize.rs`); and the per-sketch `reload_on_resize_settled` listeners (Plan 02) — a resize during `Idle` or the attract screensaver (e.g. a display re-enumerating after a TV wakes from sleep) must still respawn the sketch at the new size, so these gate on `AppState`/message-presence internally rather than on `sketch_active`.
```

- [ ] **Step 2: Verify and run the check-secrets gate**

```bash
rg -n "reload_on_resize_settled|debounce_window_resize" AGENTS.md
cargo xtask check-secrets
```

Expected: the `rg` shows the amended bullet; `check-secrets` passes (prose only, no paths/secrets introduced — `lifecycle/window_resize.rs` is a workspace-relative literal, which the checker allows).

- [ ] **Step 3: Commit**

Write this message to a scratch file, then commit:

```
docs(agents): note the window-resize listeners as always-on exceptions

AGENTS.md named only restart_on_*_settings_change as the sanctioned
always-on exception to "zero systems when idle". Plan 02 adds two more:
debounce_window_resize and the per-sketch reload_on_resize_settled
listeners, which must observe resize events in every activity (a display
re-enumerating after sleep must respawn even while idle/screensaver).
Amend the sentence so the doc matches the code.
```

```bash
git add AGENTS.md
git commit -F <scratch-message-file>
git show --stat HEAD
```

---

## Self-Review

**Locked-decision + review-directive coverage.**
- *Debounced respawn, 250 ms, new module consuming `WindowResized` AND `WindowScaleFactorChanged`, emitting `WindowResizeSettled`* → Task 1.
- *Each sketch re-runs its spawn path* → Tasks 3–4 (`reload_on_resize_settled::<S>` drives the reload overlay to re-run `OnEnter`; no spawn system edited).
- *Listener must be always-on, gating internally (review defect #1)* → Task 3 (no `run_if`; internal `**current == S::STATE && is_idle()` via `resize_reload_should_fire`) and Task 4 (registered with no `run_if`). The `sketch_active` gate is gone.
- *AGENTS.md must be amended for the new always-on class (review defect #1)* → Task 6.
- *Home hop destroys `SketchActivity`; document the consequence* → Task 3 doc + rationale, and Task 4 Step 6 wording.
- *Silent, instant resize reload via `ReloadReason` (review decision #2)* → Task 2 (`ReloadReason`, `fade_duration`/`fades_audio`, zero-duration `overlay_alpha` guard called out as load-bearing, `drive_reload_state` audio + leg-duration gating, both pure fns unit-tested).
- *(b) Cymatics sim grid re-derived from aspect* → Task 4 Step 4 (re-running `OnEnter(Cymatics)` re-runs `spawn_cymatics`, `cymatics/mod.rs:417-423`).
- *(c) dock geometry from egui, delete the `Window` query, `dock_rect` pure, tests stand (review resolution #3: `screen_rect` exists but is deprecated → use `content_rect`)* → Task 5.
- *Rescale-in-place rejected because element counts change* → Task 3 doc + commit message.

**No placeholders.** Every code block is complete; the four near-identical sketch registrations are written out in full.

**Type/signature consistency.** `WindowResizeSettled` (Task 1) → consumed by `reload_on_resize_settled` (Task 3) and defensively re-registered (Task 4). `ReloadReason` (Task 2) → consumed by `begin_fade_out` (new 5-arg signature), `restart_on_settings_change` (passes `SettingsRestart`, updated in Task 2), and `reload_on_resize_settled` (passes `WindowResize`, Task 3). `begin_fade_out`'s two pre-existing callers (the `reload.rs` test at `:218`, the `restart_on_settings_change` call at `:151`) are both updated in Task 2, so the build stays green. `resize_reload_should_fire(bool, bool, bool) -> bool` matches its call site and test. `dock_rect(f32, f32) -> (f32,f32,f32,f32)` unchanged; Task 5 changes only which two `f32`s feed it. `content_rect() -> egui::Rect`; `.width()/.height()` → f32.

**Clippy-rule audit of the example code.** No `.expect()`/`.unwrap()` in any test block (asserts + struct-literal construction only), so no `#[allow(clippy::expect_used)]` is needed. No `assert_eq!(x.is_some(), true)` (used `assert!(...is_none())` / `assert!(!out.emit)` / `assert!(a.is_finite())`). No `0..(N+1)` ranges. No underscore-binding reads. No `float_cmp`: alpha checks use `is_finite()` + `> 0.99` / `< 0.01` inequalities, and the `fade_duration`/`fades_audio` asserts compare `Duration`/`bool` (both `Eq`), not floats. No deprecated APIs (Task 5 uses `content_rect`, not the deprecated `screen_rect`). Public items (`WindowResizeSettled`, `debounce_window_resize`, `RESIZE_DEBOUNCE`, `ReloadReason`, `reload_on_resize_settled`, `SketchReloadState::{overlay_alpha,begin_fade_out}`) link only to other public items in rustdoc; the private `fade_duration`/`fades_audio`/`resize_reload_should_fire`/`debounce_step` are only *called* from bodies (allowed) and their own docs link to public items (`FADE_DURATION`) or external ones (`Duration::ZERO`).

**Dead-code note.** No transient `#![allow(dead_code)]` is needed. `ReloadReason` is `pub` (its `WindowResize` variant is also constructed in Task 2's own test), `fade_duration`/`fades_audio` are called from `overlay_alpha`/`drive_reload_state` within Task 2, `reload_on_resize_settled` is `pub` + re-exported (reachable), and `resize_reload_should_fire` is called by it.

**Ordering.** Task 1 → Task 2 (changes `begin_fade_out` signature; updates both existing callers so the build stays green) → Task 3 (adds the new caller with `ReloadReason::WindowResize`) → Task 4 (registers per sketch). Task 5 is fully independent. Task 6 (AGENTS.md) documents the always-on class Tasks 3–4 introduce; do it after Task 4. The debounce writer (Task 1, `LifecyclePlugin` `Update`) and the sketch readers (Task 4, `Update`) need no explicit `.before`/`.after`: Bevy messages are readable for two frames, so a same-frame or next-frame read is guaranteed.

**New open questions:** none. The two prior open questions are resolved: `content_rect()` is the confirmed, non-deprecated anchor (`context.rs:2823`), and Madison approved the silent-reload transition, now implemented via `ReloadReason` rather than flagged.
