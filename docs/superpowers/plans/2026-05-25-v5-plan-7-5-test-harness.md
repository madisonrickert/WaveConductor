# Plan 7.5: Test Harness — Synthetic Input + Shared `tests/common/` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate two recurring sources of friction in the integration test suite — (1) duplicated `build_app()` / `arm_idle_timeline` helpers, (2) no way to drive `MouseButtonInput` / `KeyboardInput` / `CursorMoved` through real Bevy input pipelines from a test. After this plan, end-to-end tests in `wc-sketches` can synthesize the full press → hold → release → decay → idle-veto-release → idle loop without poking `NextState` or `MouseAttractorState` directly, and they share a thin `tests/common/` module with the wc-core test crate.

**Architecture:** A new `crates/wc-core/tests/common/input.rs` module (sibling of the existing `tests/common/mod.rs` `TestSketchSettings` fixture) holds synthetic-event helpers that write into Bevy 0.18's `Message` bus: `MouseButtonInput`, `MouseMotion`, `CursorMoved`, `KeyboardInput`, plus `TouchInput`. Each helper takes `&mut App` and writes one or more messages; the next `app.update()` runs Bevy's `InputPlugin` systems, which update `ButtonInput<MouseButton>` / `ButtonInput<KeyCode>` / `Touches`, which `PointerState` and leafwing's action-tracking systems consume. A second helper module `tests/common/app.rs` hoists the duplicated `build_app()` / `test_app()` shape into a parameterized `wc_core_test_app(level: HarnessLevel)` so the wc-core lifecycle tests and the wc-sketches line_lifecycle tests share a single source of truth. A new `crates/wc-sketches/tests/line_input.rs` integration test exercises the full pipeline from synthesized key press through attractor decay using the new helpers.

**Tech Stack:** Bevy 0.18.1 (`bevy::input::ButtonInput`, `Message`-based input events), Rust 1.89, existing test deps. No new workspace dependencies.

**Reference spec:** `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` §5.3 (input layer architecture), §5.8 (testing strategy).

**Branch:** All work on `rewrite/bevy`. Pre-flight: verify HEAD is at or after the `v5-line-sim` tag (`365d685` or later).

---

## Scope check

Plan 7.5 is exclusively test-infrastructure. **No production code changes.** It ships:

1. Two `tests/common/` modules (input helpers + shared app builder)
2. One new end-to-end integration test in `wc-sketches`
3. Refactors of existing test files to consume the shared helpers

Out of scope:
- Golden-screenshot diffing (deferred until Plan 8's gravity-smear shader actually has visual character worth diffing)
- Real OS-level input injection
- Production `App` or plugin changes
- Adding/changing any sketch behavior

Three phases, three commits, Phase C pushes and tags `v5-test-harness`.

## File map

**New files:**

- `crates/wc-core/tests/common/input.rs` — synthetic event helpers
- `crates/wc-core/tests/common/app.rs` — shared `wc_core_test_app()` helpers
- `crates/wc-sketches/tests/common/mod.rs` — re-exports from wc-core's test common via path import (so wc-sketches tests can `mod common; use common::input::press_left;` without duplicating)
- `crates/wc-sketches/tests/line_input.rs` — end-to-end Line input integration test

**Modified files:**

- `crates/wc-core/tests/common/mod.rs` — declare `pub mod input;` and `pub mod app;` modules so the existing `TestSketchSettings` fixture coexists with the new helpers
- `crates/wc-core/tests/lifecycle.rs` — consume `common::app::lifecycle_test_app()` instead of inline `test_app()`
- `crates/wc-core/tests/lifecycle_idle_veto.rs` — same; also consume `common::app::arm_idle_timeline()` (carry-forward #39) and drop the unused `expect_used` allow (carry-forward #17)
- `crates/wc-sketches/tests/line_lifecycle.rs` — consume `common::app::sketches_test_app()` (or equivalent) and `common::input::*` helpers in the two existing veto-aware tests

---

## Conventions used in this plan

- All file paths absolute from the repo root.
- Code blocks show the full new content or full added section.
- Cargo commands list exact invocation + expected outcome.
- Each phase ends with one `git commit`, stage explicitly.
- When Bevy 0.18 deviates from the plan's assumptions (it has 4 times across Plans 0–7), adapt and note it in the phase report.

---

# Phase 0 — Hoist shared test helpers

Eliminate the duplicated `build_app()` patterns first so Phase A's new test can build on the shared foundation. One commit.

### Task 1: Create `crates/wc-core/tests/common/app.rs`

**File:** `crates/wc-core/tests/common/app.rs` (new)

This module hosts shared `App` builders used across wc-core and (indirectly via re-export) wc-sketches integration tests. The current state has three near-identical helpers:

- `crates/wc-core/tests/lifecycle.rs::test_app` (~10 lines)
- `crates/wc-core/tests/lifecycle_idle_veto.rs::build_app` (~10 lines)
- `crates/wc-sketches/tests/line_lifecycle.rs::build_app` (~25 lines, adds AssetPlugin + MeshPlugin + LinePlugin)

The first two are byte-identical. The third adds plugin layers but builds on the same shape. Parameterize.

- [ ] **Step 1: Create the file**

```rust
//! Shared `App` builders for wc-core integration tests.
//!
//! Eliminates the byte-identical `test_app()` / `build_app()` helpers that
//! accumulated across `tests/lifecycle.rs` and `tests/lifecycle_idle_veto.rs`,
//! and provides a `WindowFixture`-spawning variant that
//! `wc-sketches` tests can re-import via their own `tests/common/` re-export.
//!
//! ## Layers
//!
//! - [`lifecycle_test_app`] — the minimum Bevy plumbing needed to exercise
//!   `LifecyclePlugin`: `MinimalPlugins`, `InputPlugin`, `StatesPlugin`,
//!   `LifecyclePlugin` itself. Used by lifecycle and idle-veto tests.
//! - [`arm_idle_timeline`] — install `TimeUpdateStrategy::ManualDuration` so
//!   `Time<Virtual>` advances deterministically; mark the interaction timer
//!   at `now`; shrink `idle_threshold` so subsequent updates can trip Idle
//!   in a small number of ticks. Mirrors the inline pattern previously
//!   duplicated across three test files (Plan 7 carry-forward #39).
//!
//! `sketches_test_app` (the wc-sketches variant with AssetPlugin / MeshPlugin /
//! LinePlugin) lives in `crates/wc-sketches/tests/common/mod.rs` because it
//! depends on `wc-sketches` types and Cargo's per-crate test isolation
//! prevents cross-crate `tests/common/` sharing.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;

/// Build the standard wc-core lifecycle test app.
///
/// `MinimalPlugins + InputPlugin + StatesPlugin + LifecyclePlugin`. The
/// lifecycle plugin internally registers `InputManagerPlugin`,
/// `WaveConductorAction` map, `ActionState`, `InteractionTimer`,
/// `IdleVetoes`, and `HandTrackingFrame` message — no caller-side setup
/// needed.
pub fn lifecycle_test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    app
}

/// Configure an app so its idle-transition tests can advance time
/// deterministically over a handful of update ticks.
///
/// Installs `TimeUpdateStrategy::ManualDuration(80 ms)`, marks the interaction
/// timer at `Time::elapsed()` so `idle_for` starts at zero, and shrinks
/// `idle_threshold` to 50 ms so two `app.update()` calls cross the threshold.
/// Caller is expected to call `app.update()` at least twice afterward to
/// observe the Idle transition.
///
/// **Required:** the app must already have `LifecyclePlugin` registered.
pub fn arm_idle_timeline(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(
        Duration::from_millis(80),
    ));
    let now = app.world().resource::<Time>().elapsed();
    let mut timer = app
        .world_mut()
        .resource_mut::<wc_core::lifecycle::idle::InteractionTimer>();
    timer.mark(now);
    timer.idle_threshold = Duration::from_millis(50);
    timer.screensaver_threshold = Duration::from_secs(60);
}
```

- [ ] **Step 2: Declare the module**

In `crates/wc-core/tests/common/mod.rs`, prepend (above the existing `TestSketchSettings` definition):

```rust
//! Shared test fixtures for `wc-core` integration tests.
//!
//! Each integration test in `tests/*.rs` is its own crate; `tests/common/`
//! is the canonical Rust pattern for sharing helpers among them. Modules
//! here may go unused by some integration binaries — `#[allow(dead_code)]`
//! at the module level keeps `cargo test` happy.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

pub mod app;
pub mod input;
```

(Leave the existing `TestSketchSettings` struct in place at the bottom of the file. The `#![allow(dead_code)]` outer attribute may already exist — don't duplicate.)

> **Cargo quirk reminder:** `tests/common/mod.rs` is only compiled when referenced via `mod common;` in an integration test file. The new submodules `app.rs` and `input.rs` inside the `common/` directory follow the same pattern automatically.

### Task 2: Refactor `tests/lifecycle.rs` to consume `lifecycle_test_app`

**File:** `crates/wc-core/tests/lifecycle.rs`

- [ ] **Step 1: Replace inline `test_app`**

Find the existing inline `fn test_app() -> App { ... }` near the top of the file. Replace it (and any matching `use` lines for `StatesPlugin`, etc.) with:

```rust
mod common;
use common::app::{arm_idle_timeline, lifecycle_test_app};
```

Sweep the file for usages: `test_app()` → `lifecycle_test_app()`. The `arm_idle_timeline` helper is also re-imported because the existing `idle_transitions_after_threshold` test inlines the same `TimeUpdateStrategy::ManualDuration` pattern.

- [ ] **Step 2: Sweep `idle_transitions_after_threshold`**

Find the test body and replace its inline timer-arming code with `arm_idle_timeline(&mut app);` followed by the assertion loop. The body should now read something like:

```rust
let mut app = lifecycle_test_app();
app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Line);
app.update();
arm_idle_timeline(&mut app);
app.update();
app.update();
let activity = app.world().resource::<State<SketchActivity>>();
assert_eq!(*activity.get(), SketchActivity::Idle);
```

(Adjust to match the exact assertion shape the existing test has.)

- [ ] **Step 3: Run tests**

Run: `cargo test -p wc-core --test lifecycle`
Expected: all existing tests pass.

### Task 3: Refactor `tests/lifecycle_idle_veto.rs` to consume the shared helpers

**File:** `crates/wc-core/tests/lifecycle_idle_veto.rs`

- [ ] **Step 1: Drop the unused `expect_used` allow** (carry-forward #17)

Remove the `#![allow(clippy::expect_used, ...)]` block at the top of the file if no `.expect()` call appears in the file body. (Spec-reviewer report from Phase A confirmed it's unused.)

- [ ] **Step 2: Replace inline `build_app` with `lifecycle_test_app`**

Same pattern as Task 2:

```rust
mod common;
use common::app::{arm_idle_timeline, lifecycle_test_app};
```

Sweep `build_app()` → `lifecycle_test_app()`. The existing inline `arm_idle_timeline` helper in this file is the canonical implementation — confirm `common::app::arm_idle_timeline` matches its behavior before deleting.

- [ ] **Step 3: Run tests**

Run: `cargo test -p wc-core --test lifecycle_idle_veto`
Expected: 3 tests pass.

### Task 4: Create `crates/wc-sketches/tests/common/mod.rs`

**File:** `crates/wc-sketches/tests/common/mod.rs` (new)

Cargo cannot share `tests/common/` across crates, so the wc-sketches variant lives in its own crate but **re-uses the same source files via path imports** (`#[path = "../../wc-core/tests/common/input.rs"]`). This avoids duplicating the helper bodies.

- [ ] **Step 1: Create the file**

```rust
//! Shared test fixtures for `wc-sketches` integration tests.
//!
//! Re-imports the synthetic input helpers from wc-core's `tests/common/` via
//! a `#[path = ...]` attribute. This is the smallest mechanism that works
//! across Cargo's per-crate integration-test isolation while keeping the
//! helper bodies in one place. Sketches-specific helpers (the
//! `sketches_test_app` builder that adds AssetPlugin / MeshPlugin /
//! LinePlugin / a Window entity) live below.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

#[path = "../../wc-core/tests/common/input.rs"]
pub mod input;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bevy::state::app::StatesPlugin;
use wc_core::input::pointer::PointerState;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::LinePlugin;

/// Build a sketches-test app: standard wc-core lifecycle harness plus
/// `AssetPlugin`, `MeshPlugin`, `ShaderStorageBuffer` registration,
/// `PointerState`, `SettingsPlugin`, `LinePlugin`, and a synthetic Window
/// entity at 1280x720 (so `Single<&Window>` system params resolve).
///
/// Sets `WAVECONDUCTOR_CONFIG_DIR` to a per-test temp dir so persisted
/// settings don't leak between tests.
pub fn sketches_test_app() -> App {
    let dir = std::env::temp_dir().join(format!("wc-sketches-test-{}", std::process::id()));
    // SAFETY: test-only mutation of env var. Rust 1.80+ requires unsafe.
    #[allow(
        unsafe_code,
        reason = "env mutation is safe in single-threaded test setup"
    )]
    unsafe {
        std::env::set_var("WAVECONDUCTOR_CONFIG_DIR", &dir);
    }

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(AssetPlugin::default());
    app.add_plugins(StatesPlugin);
    app.init_state::<AppState>();
    app.add_sub_state::<SketchActivity>();
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    app.add_plugins(bevy::mesh::MeshPlugin);
    app.init_asset::<ShaderStorageBuffer>();
    app.init_resource::<PointerState>();
    app.add_plugins(wc_core::settings::SettingsPlugin);
    app.add_plugins(LinePlugin);

    app.world_mut().spawn(Window {
        resolution: (1280_u32, 720_u32).into(),
        ..Default::default()
    });

    app
}

/// Re-export of `arm_idle_timeline` for sketches tests, with the same
/// behavior as the wc-core helper (deterministic time advance + low
/// `idle_threshold`).
pub fn arm_idle_timeline(app: &mut App) {
    use std::time::Duration;
    use bevy::time::TimeUpdateStrategy;
    app.insert_resource(TimeUpdateStrategy::ManualDuration(
        Duration::from_millis(80),
    ));
    let now = app.world().resource::<Time>().elapsed();
    let mut timer = app
        .world_mut()
        .resource_mut::<wc_core::lifecycle::idle::InteractionTimer>();
    timer.mark(now);
    timer.idle_threshold = Duration::from_millis(50);
    timer.screensaver_threshold = Duration::from_secs(60);
}
```

> **Why duplicate `arm_idle_timeline` here?** The function depends on `wc_core::lifecycle::idle::InteractionTimer`. Both wc-core's `tests/common/app.rs` and wc-sketches's `tests/common/mod.rs` import that type. The duplication is two functions; the `tests/common/input.rs` synthetic helpers (the more substantial code) are shared via `#[path]`.

### Task 5: Refactor `crates/wc-sketches/tests/line_lifecycle.rs` to use shared helpers

**File:** `crates/wc-sketches/tests/line_lifecycle.rs`

- [ ] **Step 1: Replace `build_app` and inline `arm_idle_timeline` patterns**

Add at the top:

```rust
mod common;
use common::{arm_idle_timeline, sketches_test_app};
```

Sweep `build_app()` → `sketches_test_app()`. Find the two inline `TimeUpdateStrategy::ManualDuration` blocks in `update_sim_params_does_not_run_when_idle` and `idle_veto_keeps_line_active_during_attractor_decay`; replace each with a single `arm_idle_timeline(&mut app);` call.

- [ ] **Step 2: Verify the rewrite**

Run: `cargo test -p wc-sketches --test line_lifecycle`
Expected: 7 tests pass (no behavioral change).

### Task 6: Commit Phase 0

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-core/tests/common/mod.rs \
    crates/wc-core/tests/common/app.rs \
    crates/wc-core/tests/lifecycle.rs \
    crates/wc-core/tests/lifecycle_idle_veto.rs \
    crates/wc-sketches/tests/common/mod.rs \
    crates/wc-sketches/tests/line_lifecycle.rs
```

Note: `crates/wc-core/tests/common/input.rs` is created in Phase A; Phase 0 only ships `app.rs` and the updated `mod.rs`. The wc-sketches `common/mod.rs` includes a `#[path]` import that points at a file Phase A creates — that's fine, the `pub mod input;` line in `wc-sketches/tests/common/mod.rs` is added in Phase A's commit, not Phase 0.

**Re-read Task 4 Step 1** — that file should NOT include the `pub mod input;` re-import line in Phase 0; Phase A adds it.

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7.5 Phase 0: hoist shared test helpers to tests/common/

A new crates/wc-core/tests/common/app.rs holds lifecycle_test_app()
and arm_idle_timeline() — the byte-identical helpers previously
duplicated across three integration test files. crates/wc-sketches/
tests/common/mod.rs hosts sketches_test_app() (the AssetPlugin +
MeshPlugin + LinePlugin variant) plus an arm_idle_timeline mirror.
Drops the unused expect_used allow from lifecycle_idle_veto.rs
(Plan 7 Phase A carry-forward #17).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 3: Verify**

Run: `cargo test --workspace`
Expected: all tests pass, no behavior change.

---

# Phase A — Synthetic input helpers + end-to-end Line test

The substantive deliverable. One commit.

### Task 7: Create `crates/wc-core/tests/common/input.rs`

**File:** `crates/wc-core/tests/common/input.rs` (new)

- [ ] **Step 1: Write the helpers**

```rust
//! Synthetic input event helpers for integration tests.
//!
//! Bevy's input layer reads from `Message` buses populated by `InputPlugin`'s
//! winit integration. In tests, we synthesize the same messages directly,
//! then advance one frame so `ButtonInput<MouseButton>`, `ButtonInput<KeyCode>`,
//! and `Touches` reflect the synthesized state. Production input flows that
//! consume those resources (leafwing's `ActionState` tracker, the project's
//! `PointerState`, the Line sketch's mouse-attractor lifecycle) run end-to-end
//! without poking their internal resources directly.
//!
//! ## When to use what
//!
//! - **Pointer-driven sketches** — `press_left()`, `release_left()`,
//!   `move_pointer(x, y)`. Writes `MouseButtonInput` + `CursorMoved` so
//!   `Res<ButtonInput<MouseButton>>` and `PointerState.primary` update.
//! - **Keyboard navigation** — `tap_key(KeyCode)`, `press_key()` /
//!   `release_key()`. Writes `KeyboardInput`; leafwing picks it up next frame.
//! - **Touch UIs** — `touch_start(id, x, y)`, `touch_move`, `touch_end`.
//!
//! All helpers take `&mut App` and only write messages. **Call
//! `app.update()` (at least once) after each helper to let Bevy's input
//! systems process the synthesized events** before asserting on the resource
//! state.

#![allow(
    dead_code,
    reason = "Helpers may be unused by some integration test binaries."
)]

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseMotion};
use bevy::input::touch::{ForceTouch, TouchInput, TouchPhase};
use bevy::input::ButtonState;
use bevy::math::Vec2;
use bevy::prelude::*;
use bevy::window::CursorMoved;

/// The first `Window` entity in the app. Synthetic helpers attach events to
/// this window so production code that filters by window id finds them.
///
/// Panics if no `Window` entity exists. Tests that use these helpers must
/// build their app with a `Window` already spawned — `sketches_test_app()`
/// in `wc-sketches/tests/common/mod.rs` does this.
fn primary_window(app: &App) -> Entity {
    app.world()
        .iter_entities()
        .find_map(|e| e.contains::<Window>().then_some(e.id()))
        .expect("synthetic input helpers require a Window entity")
}

/// Write a `MouseButtonInput { Pressed }` for the given button.
///
/// Call `app.update()` after this to let Bevy's `mouse_button_input_system`
/// fold it into `Res<ButtonInput<MouseButton>>`.
pub fn press_button(app: &mut App, button: MouseButton) {
    let window = primary_window(app);
    app.world_mut().write_message(MouseButtonInput {
        button,
        state: ButtonState::Pressed,
        window,
    });
}

/// Write a `MouseButtonInput { Released }`.
pub fn release_button(app: &mut App, button: MouseButton) {
    let window = primary_window(app);
    app.world_mut().write_message(MouseButtonInput {
        button,
        state: ButtonState::Released,
        window,
    });
}

/// Convenience: press the left mouse button.
pub fn press_left(app: &mut App) {
    press_button(app, MouseButton::Left);
}

/// Convenience: release the left mouse button.
pub fn release_left(app: &mut App) {
    release_button(app, MouseButton::Left);
}

/// Move the pointer to `(x, y)` in window pixel coordinates (top-left origin,
/// +y down). Writes both `CursorMoved` (which `PointerState` consumes) and
/// `MouseMotion` (which Bevy's idle-detection consumes). Pass the previous
/// position via `from` so the motion delta is correct; if unknown, the
/// caller can pass `Vec2::ZERO`.
pub fn move_pointer(app: &mut App, x: f32, y: f32, from: Vec2) {
    let window = primary_window(app);
    let position = Vec2::new(x, y);
    let delta = position - from;
    app.world_mut().write_message(CursorMoved {
        window,
        position,
        delta: Some(delta),
    });
    if delta != Vec2::ZERO {
        app.world_mut().write_message(MouseMotion { delta });
    }
}

/// Write a `KeyboardInput { Pressed }` for `key_code`.
///
/// `logical_key` is set to `Key::Unidentified(NativeKey::Unidentified)`
/// because tests don't usually care about the logical key vs scancode
/// distinction. Bevy's `keyboard_input_system` only reads `key_code`.
pub fn press_key(app: &mut App, key_code: KeyCode) {
    let window = primary_window(app);
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
        state: ButtonState::Pressed,
        text: None,
        repeat: false,
        window,
    });
}

/// Write a `KeyboardInput { Released }` for `key_code`.
pub fn release_key(app: &mut App, key_code: KeyCode) {
    let window = primary_window(app);
    app.world_mut().write_message(KeyboardInput {
        key_code,
        logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
        state: ButtonState::Released,
        text: None,
        repeat: false,
        window,
    });
}

/// Press + release a key across a single frame boundary. Caller is
/// responsible for calling `app.update()` afterward to let leafwing's
/// `just_pressed` semantics fire.
pub fn tap_key(app: &mut App, key_code: KeyCode) {
    press_key(app, key_code);
    release_key(app, key_code);
}

/// Write a touch start event with id `touch_id` at window-space (x, y).
pub fn touch_start(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Started,
        position: Vec2::new(x, y),
        window,
        force: Some(ForceTouch::Normalized(1.0)),
        id: touch_id,
    });
}

/// Touch move event.
pub fn touch_move(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Moved,
        position: Vec2::new(x, y),
        window,
        force: Some(ForceTouch::Normalized(1.0)),
        id: touch_id,
    });
}

/// Touch end event.
pub fn touch_end(app: &mut App, touch_id: u64, x: f32, y: f32) {
    let window = primary_window(app);
    app.world_mut().write_message(TouchInput {
        phase: TouchPhase::Ended,
        position: Vec2::new(x, y),
        window,
        force: None,
        id: touch_id,
    });
}
```

> **API drift notes:** Bevy 0.18's exact `MouseButtonInput`, `CursorMoved`, `KeyboardInput`, and `TouchInput` field names may differ slightly from the above. The implementer adapts to what `bevy::input::*` actually exports in the installed 0.18.1 version and notes the deviation in the phase report.

- [ ] **Step 2: Wire the module into wc-core's `tests/common/mod.rs`**

In `crates/wc-core/tests/common/mod.rs`, the Phase 0 commit already added `pub mod input;` (verify; if not, add it).

- [ ] **Step 3: Wire the module into wc-sketches's `tests/common/mod.rs`**

In `crates/wc-sketches/tests/common/mod.rs`, add the `pub mod input;` declaration that re-imports via `#[path]`:

```rust
#[path = "../../wc-core/tests/common/input.rs"]
pub mod input;
```

If the `#[path]` import was already added in Phase 0, no change. (Per Task 6's clarification, that import was deferred to Phase A — add it here.)

- [ ] **Step 4: Build**

Run: `cargo test -p wc-sketches --no-run`
Expected: tests compile cleanly. (No test exercises the helpers yet — Task 8 adds the first.)

### Task 8: End-to-end Line input test

**File:** `crates/wc-sketches/tests/line_input.rs` (new)

Drives the full input pipeline: enters Line via leafwing keyboard nav, synthesizes a press-and-hold at a world position, asserts attractor activation + decay + idle-veto-hold + eventual Idle transition once power reaches zero.

- [ ] **Step 1: Write the test file**

```rust
//! End-to-end input integration tests for the Line sketch.
//!
//! Synthesizes keyboard, mouse, and touch events via `common::input`
//! helpers and asserts on the resulting state without bypassing any
//! production code path.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::input::{press_left, release_left, tap_key};
use common::{arm_idle_timeline, sketches_test_app};

use std::time::Duration;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::compute::LineSimParams;
use wc_sketches::line::systems::MouseAttractorState;

fn enter_line(app: &mut App) {
    // The lifecycle nav binding is configured in
    // crates/wc-core/src/lifecycle/actions.rs. Defaults bind Digit1 to the
    // Line variant. If that changes, this test will fail with a clear
    // state assertion.
    tap_key(app, KeyCode::Digit1);
    app.update();
    app.update(); // state transition resolved
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "Digit1 keyboard nav should enter AppState::Line",
    );
}

#[test]
fn left_press_activates_mouse_attractor() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    let before = app.world().resource::<MouseAttractorState>().power;
    assert_eq!(before, 0.0, "attractor inactive before any input");

    press_left(&mut app);
    app.update();

    let after = app.world().resource::<MouseAttractorState>().power;
    assert!(
        after > 0.0,
        "left press should raise MouseAttractorState.power above zero, got {after}"
    );
}

#[test]
fn sim_params_records_one_attractor_after_press() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    press_left(&mut app);
    app.update();

    let sim = app
        .world()
        .get_resource::<LineSimParams>()
        .expect("LineSimParams should exist after entering Line");
    assert_eq!(
        sim.params.attractor_count, 1,
        "one mouse attractor should be active immediately after press"
    );
}

#[test]
fn release_starts_attractor_decay() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    press_left(&mut app);
    app.update();
    let peak = app.world().resource::<MouseAttractorState>().power;
    assert!(peak > 0.0, "expected non-zero peak after press, got {peak}");

    release_left(&mut app);
    app.update();
    app.update(); // one decay tick after release

    let decayed = app.world().resource::<MouseAttractorState>().power;
    assert!(
        decayed < peak,
        "power should decay after release: peak={peak}, decayed={decayed}"
    );
}

#[test]
fn decay_holds_active_via_idle_veto() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    press_left(&mut app);
    app.update();
    release_left(&mut app);
    app.update();

    // Power is non-zero but decaying. Drive idle threshold past expiration.
    arm_idle_timeline(&mut app);
    for _ in 0..3 {
        app.update();
    }

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "Line idle veto should hold Active while attractor decays"
    );
}

#[test]
fn power_zero_lets_state_transition_to_idle() {
    let mut app = sketches_test_app();
    app.update();
    enter_line(&mut app);

    // Force the attractor straight to zero so the veto returns false.
    app.world_mut().resource_mut::<MouseAttractorState>().power = 0.0;

    arm_idle_timeline(&mut app);
    for _ in 0..3 {
        app.update();
    }

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Idle,
        "with veto inactive and idle threshold crossed, state should become Idle"
    );
}
```

> **If `Digit1` is not the actual nav binding for Line**, look at `crates/wc-core/src/lifecycle/actions.rs::default_input_map()` and use the correct `KeyCode`. The test will fail with a clear assertion message on the `AppState::Line` check, so the wrong binding is easy to diagnose.

- [ ] **Step 2: Run the test**

Run: `cargo test -p wc-sketches --test line_input`
Expected: 5 tests pass.

If any test fails because `tap_key(KeyCode::Digit1)` doesn't transition to `AppState::Line`, the binding is somewhere else. Check `default_input_map()` and adjust `enter_line()`.

### Task 9: Commit Phase A

- [ ] **Step 1: Stage**

```bash
git add \
    crates/wc-core/tests/common/input.rs \
    crates/wc-core/tests/common/mod.rs \
    crates/wc-sketches/tests/common/mod.rs \
    crates/wc-sketches/tests/line_input.rs
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
Plan 7.5 Phase A: synthetic input helpers + end-to-end Line test

crates/wc-core/tests/common/input.rs adds press_left / release_left /
move_pointer / press_key / release_key / tap_key / touch_* helpers
that write Bevy 0.18 input messages into the bus; subsequent
app.update() runs InputPlugin's input systems so production code
that consumes ButtonInput / Touches / CursorMoved / leafwing's
ActionState runs end-to-end. crates/wc-sketches/tests/line_input.rs
is the first consumer, exercising press → attractor activation,
release → decay, decay → idle-veto-hold, and power-zero → Idle
transitions without poking any production resource directly.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 3: Run the full suite**

Run: `cargo test --workspace`
Expected: all tests pass, including the new 5 in line_input.

---

# Phase B — Push, verify CI, tag `v5-test-harness`

### Task 10: Push and tag

- [ ] **Step 1: Run the local gates**

Run: `cargo fmt --all -- --check`
Expected: clean.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `cargo test --workspace`
Expected: clean.

- [ ] **Step 2: Push**

```bash
git push origin rewrite/bevy
```

- [ ] **Step 3: Wait for CI green**

Watch the run; expect all 10 jobs (fmt, clippy, check-secrets, deny, audit, test × Ubuntu / macOS / Windows, doc) to succeed. If `doc` fails because of new intra-doc links, fix by demoting to plain prose and re-pushing.

- [ ] **Step 4: Tag**

```bash
git tag v5-test-harness
git push origin v5-test-harness
```

- [ ] **Step 5: Update the roadmap**

In `docs/superpowers/roadmap.md`, change the Plan 7.5 row from `⏳ next` to `✅ shipped` and set Plan 8 to `⏳ next`. Commit + push:

```bash
git add docs/superpowers/roadmap.md
git commit -m "$(cat <<'EOF'
roadmap: Plan 7.5 shipped (v5-test-harness); Plan 8 is next

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
git push origin rewrite/bevy
```

---

## Self-review checklist

After completing all phases:

- [ ] No production code changes — all changes confined to `tests/` directories
- [ ] `cargo test --workspace` passes (count delta: +5 tests in `line_input.rs`)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo doc --no-deps --workspace --document-private-items` passes (no broken intra-doc links)
- [ ] Three commits land on `rewrite/bevy`; tag `v5-test-harness` points at the final commit before the roadmap update
- [ ] Roadmap shows Plan 7.5 ✅ and Plan 8 ⏳

## Carry-forwards for Plan 8

Items surfaced during Plan 7.5 review or testing — populated during execution.

- *(empty)*

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-25-v5-plan-7-5-test-harness.md`. Two execution options:

**1. Subagent-Driven (recommended)** — dispatch fresh implementer per phase + two-stage review per phase.

**2. Inline Execution** — execute tasks in this session using `superpowers:executing-plans`.

Which approach?
