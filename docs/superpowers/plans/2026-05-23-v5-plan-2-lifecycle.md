# v5 Plan 2: Lifecycle + Housekeeping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Bevy `States` + `SubStates` machinery for sketch selection and idle/screensaver, wire `leafwing-input-manager` for keyboard shortcuts, plus resolve the Plan 1 follow-up housekeeping items.

**Architecture:** A new `lifecycle/` module in `wc-core` exposing `AppState` (Home, Line, Flame, Dots, Cymatics, Waves) and a `SketchActivity` SubState (Active, Idle, Screensaver). A `WaveConductorAction` enum bound to physical keys via `leafwing-input-manager` drives state transitions. Sketch update systems will gate on `SketchActivity::Active` once they exist; for now the lifecycle is exercised through integration tests and visible log lines.

**Tech Stack:** Bevy 0.18.1, `leafwing-input-manager` 0.16, Rust 1.89, existing workspace from Plan 1.

**Reference spec:** `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` §5.2 (lifecycle), §5.3 (the leafwing keyboard portion).

**Branch:** All work on `rewrite/bevy`. Do not touch `main`.

**Pre-flight check:** before starting, verify you are on commit `dd58308` or later (head of `rewrite/bevy` post-Plan-1).

---

## File map

**Created in this plan:**

- `crates/wc-core/src/lifecycle/mod.rs` — module root, `LifecyclePlugin` definition
- `crates/wc-core/src/lifecycle/state.rs` — `AppState` enum + `SketchActivity` SubState
- `crates/wc-core/src/lifecycle/idle.rs` — `InteractionTimer` resource + idle detection
- `crates/wc-core/src/lifecycle/screensaver.rs` — Screensaver overlay marker + show/hide systems
- `crates/wc-core/src/lifecycle/actions.rs` — `WaveConductorAction` enum + leafwing `InputMap`
- `crates/wc-core/src/lifecycle/nav.rs` — Navigation handler (responds to actions → state transitions)
- `crates/wc-core/tests/lifecycle.rs` — integration tests (state machine + actions)

**Modified in this plan:**

- `crates/wc-core/Cargo.toml` — add `leafwing-input-manager` workspace dependency
- `crates/wc-core/src/lib.rs` — register `LifecyclePlugin` inside `CorePlugin`
- `crates/waveconductor/src/main.rs` — small `tracing` subscriber init so state transitions log visibly during manual testing
- `crates/waveconductor/Cargo.toml` — add `tracing-subscriber` dependency
- `Cargo.toml` — add `tracing-subscriber` to `[workspace.dependencies]`
- `.github/workflows/ci.yml` — add `--all-features` to clippy gate
- `crates/waveconductor/Cargo.toml`, `crates/wc-core/Cargo.toml`, `crates/wc-sketches/Cargo.toml` — add `publish = false`
- `crates/wc-core/src/lib.rs`, `crates/wc-sketches/src/lib.rs` — remove redundant `#![warn(missing_docs)]` (workspace lints already enforce it)
- `xtask/src/manifest.rs` — add a comment noting the coupling with `main.rs`'s subcommand enum
- `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md` — update §5.1 to use `assets/app-icons/`
- `README.md` — add Linux build prerequisites

**Deleted in this plan:**

- `ADL.md` — v4-era architecture decision log; superseded by `docs/adr/`

---

## Phase 0: Housekeeping

These are the items the Plan 1 final reviewer carried forward. Each is small and independent.

### Task 1: Add `publish = false` to internal crates

**Files:**
- Modify: `crates/waveconductor/Cargo.toml`, `crates/wc-core/Cargo.toml`, `crates/wc-sketches/Cargo.toml`

- [ ] **Step 1: Add `publish = false` under `[package]` in each crate**

In each of the three Cargo.toml files, add `publish = false` immediately after the `description` line in the `[package]` block. Example for `crates/waveconductor/Cargo.toml`:

```toml
[package]
name = "waveconductor"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "WaveConductor v5 — Rust + Bevy interactive art gallery."
publish = false
```

Repeat for `crates/wc-core/Cargo.toml` and `crates/wc-sketches/Cargo.toml`.

- [ ] **Step 2: Verify the manifests still parse**

Run:
```bash
cargo metadata --no-deps --format-version 1 > /dev/null && echo OK
```

Expected: `OK`.

### Task 2: Remove redundant `#![warn(missing_docs)]` from library crates

**Files:**
- Modify: `crates/wc-core/src/lib.rs`, `crates/wc-sketches/src/lib.rs`

- [ ] **Step 1: Remove the redundant inner attribute**

The workspace `[workspace.lints.rust]` table already sets `missing_docs = "warn"`. Crate-level `#![warn(missing_docs)]` is duplicative.

In `crates/wc-core/src/lib.rs`, find and delete the line:

```rust
#![warn(missing_docs)]
```

In `crates/wc-sketches/src/lib.rs`, find and delete the same line.

- [ ] **Step 2: Verify nothing regressed**

Run:
```bash
cargo clippy --all-targets --workspace -- -D warnings 2>&1 | tail -5
```

Expected: clean (no new warnings).

### Task 3: Remove `ADL.md`

**Files:**
- Delete: `ADL.md`

- [ ] **Step 1: Verify `ADL.md` is v4-era**

```bash
head -5 ADL.md
```

It should be an architecture decision log mentioning TypeScript or React. The replacement is `docs/adr/`.

- [ ] **Step 2: Delete**

```bash
rm ADL.md
```

### Task 4: Document `xtask` manifest coupling

**Files:**
- Modify: `xtask/src/manifest.rs`

- [ ] **Step 1: Add a coupling comment**

In `xtask/src/manifest.rs`, find the `SUBCOMMANDS` const (or equivalent — it may be named `ENTRIES`). Immediately above its declaration, add this comment block:

```rust
// Hand-maintained list of xtask subcommands. The `Command` enum in `main.rs` is
// the authoritative source for what subcommands exist; this table mirrors that
// list for the human-readable manifest output. If you add a subcommand to
// `main.rs`, you MUST add an entry here too, or the manifest will silently
// diverge from the real command surface.
```

The exact name of the const may differ; add the comment immediately above whichever array/slice holds the human-readable entries.

- [ ] **Step 2: Verify build still passes**

```bash
cargo build -p xtask 2>&1 | tail -3
```

Expected: succeeds.

### Task 5: Update spec §5.1 to use `assets/app-icons`

**Files:**
- Modify: `docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md`

- [ ] **Step 1: Replace `assets/icons` with `assets/app-icons` in the spec workspace tree**

Find the line in §5.1 that says:

```
│   └── icons/
```

(inside the `assets/` block). Replace with:

```
│   └── app-icons/                          # renamed from "icons" — macOS global gitignore `Icon?` matches "icons" case-insensitively
```

The annotation tells future readers why the rename happened.

- [ ] **Step 2: Verify nothing else references `assets/icons`**

```bash
grep -n "assets/icons" docs/superpowers/specs/2026-05-22-bevy-rewrite-design.md docs/superpowers/plans/*.md
```

Expected: only the new line in the spec. If matches in the plan files appear, leave them — plan docs are historical and freezing them is fine.

### Task 6: Add `--all-features` to CI clippy gate

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Update the clippy job command**

In `.github/workflows/ci.yml`, find the clippy job and change:

```yaml
      - run: cargo clippy --all-targets --workspace -- -D warnings
```

to:

```yaml
      - run: cargo clippy --all-targets --all-features --workspace -- -D warnings
```

This aligns CI with spec §5.10 and prepares for feature-gated code arriving in later plans.

- [ ] **Step 2: Run clippy locally with the new flags**

```bash
cargo clippy --all-targets --all-features --workspace -- -D warnings 2>&1 | tail -5
```

Expected: clean.

### Task 7: Add Linux contributor prerequisites to README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a Linux prerequisites note**

In `README.md`, find the "Development (v5)" section. After the `cargo run -p waveconductor` block and the "Requires Rust 1.85+" line, add:

```markdown
### Linux build prerequisites

On Debian/Ubuntu, install Bevy's native dependencies:

```sh
sudo apt-get install -y \
    libasound2-dev libudev-dev \
    libwayland-dev libxkbcommon-dev \
    libx11-dev libxcursor-dev libxi-dev libxrandr-dev
```

macOS and Windows have no extra prerequisites beyond Rust.
```

(Note: the inner code block is fenced; preserve fencing carefully.)

Also fix the stale `Rust 1.85+` line to `Rust 1.89+` since the Plan 1 toolchain bump landed but the README still says 1.85.

### Task 8: Commit Phase 0 housekeeping

**Files:** none (commit only)

- [ ] **Step 1: Verify all local gates pass**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings 2>&1 | tail -3
cargo test --workspace 2>&1 | tail -5
cargo xtask check-secrets 2>&1 | tail -3
```

Expected: all clean.

- [ ] **Step 2: Stage and commit**

```bash
git add -A
git status --short
git commit -m "$(cat <<'EOF'
Plan 1 housekeeping: publish=false, redundant lints, docs

Resolves the follow-up items surfaced by the Plan 1 final reviewer:
- publish = false on the three internal crates
- remove redundant #![warn(missing_docs)] in lib crates
- delete v4-era ADL.md (superseded by docs/adr/)
- document xtask manifest/Command coupling
- update spec §5.1 to assets/app-icons
- add --all-features to CI clippy
- add Linux prerequisites + correct Rust version in README

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase A: Lifecycle scaffolding

### Task 9: Add `leafwing-input-manager` and `tracing-subscriber` to workspace deps

**Files:**
- Modify: `Cargo.toml` (root), `crates/wc-core/Cargo.toml`, `crates/waveconductor/Cargo.toml`

- [ ] **Step 1: Verify `leafwing-input-manager` is already in `[workspace.dependencies]`**

```bash
grep -n "leafwing-input-manager" Cargo.toml
```

Expected: one match showing `leafwing-input-manager = "0.16"` (added in Plan 1's foundation commit).

If absent, add it under `[workspace.dependencies]` with `leafwing-input-manager = "0.16"`.

- [ ] **Step 2: Add `tracing-subscriber` to `[workspace.dependencies]`**

In root `Cargo.toml`, add to the `[workspace.dependencies]` block (alphabetical order around the other tracing entries):

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

- [ ] **Step 3: Add `leafwing-input-manager` to `crates/wc-core/Cargo.toml` `[dependencies]`**

Change the `[dependencies]` block to include:

```toml
[dependencies]
bevy = { workspace = true }
leafwing-input-manager = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 4: Add `tracing-subscriber` to `crates/waveconductor/Cargo.toml`**

Change the `[dependencies]` block to include:

```toml
[dependencies]
bevy = { workspace = true }
wc-core = { workspace = true }
wc-sketches = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 5: Verify the workspace still resolves**

```bash
cargo metadata --no-deps --format-version 1 > /dev/null && echo OK
cargo build --workspace 2>&1 | tail -5
```

Expected: `OK` from metadata; build succeeds (may need to download leafwing/tracing-subscriber).

If `leafwing-input-manager` 0.16 cannot find a version compatible with Bevy 0.18, check leafwing's CHANGELOG (https://github.com/Leafwing-Studios/leafwing-input-manager/blob/main/CHANGELOG.md) for the correct version that targets Bevy 0.18 and update the workspace pin accordingly. As of the spec date, leafwing 0.16 is the target; if the implementer finds a later version (e.g. 0.17) that is the actual Bevy-0.18-compatible release, bump the workspace pin.

### Task 10: Create `lifecycle` module skeleton

**Files:**
- Create: `crates/wc-core/src/lifecycle/mod.rs`
- Modify: `crates/wc-core/src/lib.rs`

- [ ] **Step 1: Create the module directory**

```bash
mkdir -p crates/wc-core/src/lifecycle
```

- [ ] **Step 2: Create `lifecycle/mod.rs` with the plugin skeleton**

Create `crates/wc-core/src/lifecycle/mod.rs` with this exact content:

```rust
//! Lifecycle subsystem: app-level navigation states, sketch activity sub-states,
//! idle detection, screensaver overlay, and the keyboard-action input map that
//! drives navigation.
//!
//! ## Data flow
//!
//! 1. User presses a key bound by [`actions::WaveConductorAction`].
//! 2. `leafwing-input-manager` updates `Res<ActionState<WaveConductorAction>>`.
//! 3. [`nav::handle_navigation_actions`] reads the action state and transitions
//!    [`state::AppState`] via `NextState<AppState>`.
//! 4. Any interaction (mouse, keyboard, future hand-tracking) resets
//!    [`idle::InteractionTimer`].
//! 5. The idle system advances [`state::SketchActivity`] through Active → Idle →
//!    Screensaver as the timer crosses configured thresholds.
//!
//! Sketches (registered in `wc-sketches`) gate their update systems on
//! `in_state(SketchActivity::Active)` so they stop simulating when idle.

pub mod actions;
pub mod idle;
pub mod nav;
pub mod screensaver;
pub mod state;

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

/// Single plugin that wires every lifecycle subsystem into the Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`].
pub struct LifecyclePlugin;

impl Plugin for LifecyclePlugin {
    fn build(&self, app: &mut App) {
        app
            // States machine
            .init_state::<state::AppState>()
            .add_sub_state::<state::SketchActivity>()
            // Input action mapping (leafwing)
            .add_plugins(InputManagerPlugin::<actions::WaveConductorAction>::default())
            .insert_resource(actions::default_input_map())
            .init_resource::<ActionState<actions::WaveConductorAction>>()
            // Idle / interaction tracking
            .init_resource::<idle::InteractionTimer>()
            // Systems
            .add_systems(
                Update,
                (
                    nav::handle_navigation_actions,
                    idle::reset_on_interaction,
                    idle::advance_activity,
                )
                    .chain(),
            )
            .add_systems(OnEnter(state::SketchActivity::Screensaver), screensaver::show)
            .add_systems(OnExit(state::SketchActivity::Screensaver), screensaver::hide);
    }
}
```

- [ ] **Step 3: Register `LifecyclePlugin` in `CorePlugin`**

Update `crates/wc-core/src/lib.rs` to add the lifecycle module and register the plugin:

```rust
//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, audio, input, settings,
//! and math helpers. Sketches consume this crate via [`CorePlugin`]; the binary
//! crate registers `CorePlugin` once at app startup.

pub mod lifecycle;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate. As subsystems land in later plans, they
/// are added as sub-plugins inside this `build()` method.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_plugin_builds_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(CorePlugin);
    }
}
```

Note: the previous version of `core_plugin_builds_without_panicking` used `App::new()` alone. Adding `MinimalPlugins` is necessary because `LifecyclePlugin` registers states which require certain Bevy infrastructure. `MinimalPlugins` provides a headless subset suitable for tests.

- [ ] **Step 4: Verify compilation (will fail until subsequent tasks add stubs for `state`, `idle`, `actions`, `nav`, `screensaver`)**

This task is allowed to leave the workspace temporarily uncompilable. The next 5 tasks add the referenced modules. Do NOT commit until Task 17 (Phase A verification).

### Task 11: `AppState` and `SketchActivity`

**Files:**
- Create: `crates/wc-core/src/lifecycle/state.rs`

- [ ] **Step 1: Create the state module**

Create `crates/wc-core/src/lifecycle/state.rs` with this exact content:

```rust
//! Top-level app navigation [`AppState`] and the sketch-active [`SketchActivity`]
//! sub-state. The state machine sits at the heart of the lifecycle plugin and
//! gates every sketch's update systems.

use bevy::prelude::*;

/// Which top-level scene is active.
///
/// The home screen is the default; selecting a sketch transitions out of `Home`
/// and into the matching variant. Pressing Escape returns to `Home`.
#[derive(States, Default, Clone, Eq, PartialEq, Hash, Debug)]
#[allow(missing_docs, reason = "variant names are self-documenting")]
pub enum AppState {
    #[default]
    Home,
    Line,
    Flame,
    Dots,
    Cymatics,
    Waves,
}

impl AppState {
    /// Stable ordering of the sketch variants, used by Next/Previous navigation.
    ///
    /// `Home` is not part of the cycle; it is the entry/exit point only.
    pub const SKETCH_ORDER: [Self; 5] = [
        Self::Line,
        Self::Flame,
        Self::Dots,
        Self::Cymatics,
        Self::Waves,
    ];

    /// Whether this state represents an active sketch (i.e. not `Home`).
    #[must_use]
    pub fn is_sketch(self) -> bool {
        !matches!(self, Self::Home)
    }

    /// The next sketch in [`SKETCH_ORDER`]; wraps around. Returns `Self::Line`
    /// when called on `Home`.
    #[must_use]
    pub fn next_sketch(self) -> Self {
        if self == Self::Home {
            return Self::SKETCH_ORDER[0];
        }
        let idx = Self::SKETCH_ORDER
            .iter()
            .position(|s| *s == self)
            .unwrap_or(0);
        Self::SKETCH_ORDER[(idx + 1) % Self::SKETCH_ORDER.len()]
    }

    /// The previous sketch in [`SKETCH_ORDER`]; wraps around. Returns the last
    /// sketch when called on `Home`.
    #[must_use]
    pub fn prev_sketch(self) -> Self {
        if self == Self::Home {
            return Self::SKETCH_ORDER[Self::SKETCH_ORDER.len() - 1];
        }
        let idx = Self::SKETCH_ORDER
            .iter()
            .position(|s| *s == self)
            .unwrap_or(0);
        let len = Self::SKETCH_ORDER.len();
        Self::SKETCH_ORDER[(idx + len - 1) % len]
    }
}

/// Whether the currently-active sketch is simulating, idle, or showing the
/// screensaver overlay. Only meaningful when [`AppState`] is a sketch (not
/// `Home`); the sub-state is gated to the sketch variants by Bevy.
#[derive(SubStates, Default, Clone, Eq, PartialEq, Hash, Debug)]
#[source(AppState = AppState::Line | AppState::Flame | AppState::Dots
                  | AppState::Cymatics | AppState::Waves)]
#[allow(missing_docs, reason = "variant names are self-documenting")]
pub enum SketchActivity {
    #[default]
    Active,
    Idle,
    Screensaver,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_sketch_wraps() {
        assert_eq!(AppState::Line.next_sketch(), AppState::Flame);
        assert_eq!(AppState::Waves.next_sketch(), AppState::Line);
    }

    #[test]
    fn prev_sketch_wraps() {
        assert_eq!(AppState::Flame.prev_sketch(), AppState::Line);
        assert_eq!(AppState::Line.prev_sketch(), AppState::Waves);
    }

    #[test]
    fn home_navigation_returns_to_endpoints() {
        assert_eq!(AppState::Home.next_sketch(), AppState::Line);
        assert_eq!(AppState::Home.prev_sketch(), AppState::Waves);
    }

    #[test]
    fn is_sketch_excludes_home() {
        assert!(!AppState::Home.is_sketch());
        for s in AppState::SKETCH_ORDER {
            assert!(s.is_sketch(), "{s:?} should be a sketch");
        }
    }
}
```

- [ ] **Step 2: Run the unit tests**

```bash
cargo test -p wc-core lifecycle::state::tests 2>&1 | tail -10
```

Expected: 4 tests pass.

If the `SubStates` derive fails to compile with the `#[source(AppState = AppState::Line | ...)]` syntax, check the Bevy 0.18 SubStates documentation (https://docs.rs/bevy/0.18/bevy/state/state/derive.SubStates.html) for the current attribute form. If the syntax is `#[source(AppState = AppState::Line)]` (single variant per attribute) or `#[source(state = AppState, in = ...)]` etc., adapt accordingly. The intent is: `SketchActivity` is a sub-state that exists only while `AppState` matches one of the five sketch variants.

### Task 12: `WaveConductorAction` enum + InputMap

**Files:**
- Create: `crates/wc-core/src/lifecycle/actions.rs`

- [ ] **Step 1: Create the actions module**

Create `crates/wc-core/src/lifecycle/actions.rs` with this exact content:

```rust
//! Keyboard action mapping driven by `leafwing-input-manager`.
//!
//! The [`WaveConductorAction`] enum is the abstract action surface that the
//! lifecycle plugin consumes. The physical keys are bound here via
//! [`default_input_map`]; future settings UI can rebind by editing the
//! `InputMap` resource.

use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

/// Top-level keyboard actions used by [`crate::lifecycle::nav`] to drive
/// [`crate::lifecycle::state::AppState`] transitions and global UI toggles.
#[derive(Actionlike, Reflect, Clone, Copy, Hash, PartialEq, Eq, Debug)]
#[reflect(Hash, PartialEq)]
pub enum WaveConductorAction {
    /// Cycle to the previous sketch (`z` / `←`).
    NavigatePrev,
    /// Cycle to the next sketch (`x` / `→`).
    NavigateNext,
    /// Jump directly to sketch 1 — Line (`1`).
    SelectLine,
    /// Jump to sketch 2 — Flame (`2`).
    SelectFlame,
    /// Jump to sketch 3 — Dots (`3`).
    SelectDots,
    /// Jump to sketch 4 — Cymatics (`4`).
    SelectCymatics,
    /// Jump to sketch 5 — Waves (`5`).
    SelectWaves,
    /// Return to the home gallery (`Escape`).
    NavigateHome,
    /// Toggle global volume (`V`). Wired in Plan 4 (audio).
    ToggleVolume,
    /// Toggle the developer settings panel (`Shift+D`). Wired in Plan 5 (settings).
    ToggleDevPanel,
    /// Toggle fullscreen (`F11`). Handled by the lifecycle plugin.
    ToggleFullscreen,
    /// Quit the application (`Alt+F4` on Windows, `Cmd+Q` on macOS).
    Quit,
}

/// Build the default `InputMap<WaveConductorAction>` matching v4's hotkey table.
///
/// Returned as a `Resource` so the lifecycle plugin can register it and a future
/// settings panel can mutate it.
#[must_use]
pub fn default_input_map() -> InputMap<WaveConductorAction> {
    use KeyCode::*;
    use WaveConductorAction as A;

    let mut map = InputMap::default();

    // Sketch selection
    map.insert(A::SelectLine, Digit1);
    map.insert(A::SelectFlame, Digit2);
    map.insert(A::SelectDots, Digit3);
    map.insert(A::SelectCymatics, Digit4);
    map.insert(A::SelectWaves, Digit5);

    // Sequential navigation
    map.insert(A::NavigatePrev, KeyZ);
    map.insert(A::NavigatePrev, ArrowLeft);
    map.insert(A::NavigateNext, KeyX);
    map.insert(A::NavigateNext, ArrowRight);

    // Global toggles
    map.insert(A::NavigateHome, Escape);
    map.insert(A::ToggleVolume, KeyV);
    map.insert(A::ToggleFullscreen, F11);

    // Modifier combos
    map.insert_modified(A::ToggleDevPanel, ModifierKey::Shift, KeyD);

    // Platform-specific quit handled in the nav system; default binding here is
    // Cmd+Q (Apple) and Ctrl+Q (others). leafwing's portable modifier helper:
    map.insert_modified(A::Quit, ModifierKey::Control, KeyQ);

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_input_map_contains_all_actions() {
        let map = default_input_map();
        // Every variant should have at least one binding.
        for action in [
            WaveConductorAction::NavigatePrev,
            WaveConductorAction::NavigateNext,
            WaveConductorAction::SelectLine,
            WaveConductorAction::SelectFlame,
            WaveConductorAction::SelectDots,
            WaveConductorAction::SelectCymatics,
            WaveConductorAction::SelectWaves,
            WaveConductorAction::NavigateHome,
            WaveConductorAction::ToggleVolume,
            WaveConductorAction::ToggleDevPanel,
            WaveConductorAction::ToggleFullscreen,
            WaveConductorAction::Quit,
        ] {
            assert!(
                map.get_buttonlike(&action).is_some(),
                "no binding for {action:?}",
            );
        }
    }
}
```

- [ ] **Step 2: Run the unit tests**

```bash
cargo test -p wc-core lifecycle::actions::tests 2>&1 | tail -10
```

Expected: 1 test passes.

If leafwing's API has drifted (e.g., `InputMap::insert` takes different arg order, or `insert_modified` doesn't exist), consult the version of leafwing actually resolved by Cargo via `cargo doc -p leafwing-input-manager --open` or by browsing https://docs.rs/leafwing-input-manager. The intent is: bind each `WaveConductorAction` variant to the physical key(s) listed in the v4 README. Treat the exact API form as a portability hazard, not the structure.

### Task 13: Idle detection and `InteractionTimer`

**Files:**
- Create: `crates/wc-core/src/lifecycle/idle.rs`

- [ ] **Step 1: Create the idle module**

Create `crates/wc-core/src/lifecycle/idle.rs` with this exact content:

```rust
//! Interaction tracking and idle / screensaver state transitions.
//!
//! The `InteractionTimer` resource records the time of the last detected
//! interaction. Two systems drive its evolution:
//!
//! - [`reset_on_interaction`] resets the timer whenever any input event
//!   (mouse, keyboard, touch) is observed.
//! - [`advance_activity`] reads the timer each frame and transitions
//!   [`crate::lifecycle::state::SketchActivity`] through
//!   `Active → Idle → Screensaver` as the elapsed time crosses thresholds.

use std::time::Duration;

use bevy::prelude::*;

use super::state::SketchActivity;

/// Tracks when the user last interacted with the app, plus the thresholds at
/// which the lifecycle plugin transitions the sketch activity state.
#[derive(Resource, Debug, Clone)]
pub struct InteractionTimer {
    /// Time of last detected interaction, in `Res<Time>::elapsed()` units.
    last_interaction: Duration,
    /// After this much idle time, transition `Active → Idle`.
    pub idle_threshold: Duration,
    /// After this much idle time, transition `Idle → Screensaver`.
    pub screensaver_threshold: Duration,
}

impl Default for InteractionTimer {
    fn default() -> Self {
        Self {
            last_interaction: Duration::ZERO,
            // Both default to 30 s per v4 BaseSketch.ts.
            idle_threshold: Duration::from_secs(30),
            screensaver_threshold: Duration::from_secs(30),
        }
    }
}

impl InteractionTimer {
    /// Record that an interaction just happened.
    pub fn mark(&mut self, now: Duration) {
        self.last_interaction = now;
    }

    /// Seconds elapsed since the last interaction.
    #[must_use]
    pub fn idle_for(&self, now: Duration) -> Duration {
        now.saturating_sub(self.last_interaction)
    }
}

/// Resets [`InteractionTimer`] whenever any input event is observed.
///
/// Reads mouse, keyboard, and touch event streams. Hand-tracking will be added
/// in Plan 3 (Input system) by also watching `Events<HandTrackingFrame>`.
pub fn reset_on_interaction(
    time: Res<Time>,
    mut timer: ResMut<InteractionTimer>,
    mouse_motion: EventReader<bevy::input::mouse::MouseMotion>,
    mouse_buttons: EventReader<bevy::input::mouse::MouseButtonInput>,
    keyboard: EventReader<bevy::input::keyboard::KeyboardInput>,
    touch: EventReader<bevy::input::touch::TouchInput>,
) {
    let any_event = !mouse_motion.is_empty()
        || !mouse_buttons.is_empty()
        || !keyboard.is_empty()
        || !touch.is_empty();
    if any_event {
        timer.mark(time.elapsed());
    }
}

/// Reads [`InteractionTimer`] and transitions [`SketchActivity`] when the
/// configured thresholds are crossed.
///
/// Only runs when in a sketch state (the sub-state itself is gated by the
/// `#[source]` annotation on `SketchActivity`).
pub fn advance_activity(
    time: Res<Time>,
    timer: Res<InteractionTimer>,
    current: Res<State<SketchActivity>>,
    mut next: ResMut<NextState<SketchActivity>>,
) {
    let idle = timer.idle_for(time.elapsed());
    let target = if idle >= timer.screensaver_threshold + timer.idle_threshold {
        SketchActivity::Screensaver
    } else if idle >= timer.idle_threshold {
        SketchActivity::Idle
    } else {
        SketchActivity::Active
    };
    if *current.get() != target {
        next.set(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_for_handles_clock_resets() {
        let mut timer = InteractionTimer::default();
        timer.mark(Duration::from_secs(10));
        // Querying with an earlier "now" should saturate to zero, not panic.
        assert_eq!(timer.idle_for(Duration::from_secs(5)), Duration::ZERO);
    }

    #[test]
    fn idle_for_reports_elapsed() {
        let mut timer = InteractionTimer::default();
        timer.mark(Duration::from_secs(10));
        assert_eq!(timer.idle_for(Duration::from_secs(45)), Duration::from_secs(35));
    }

    #[test]
    fn defaults_match_v4_thirty_second_idle() {
        let timer = InteractionTimer::default();
        assert_eq!(timer.idle_threshold, Duration::from_secs(30));
        assert_eq!(timer.screensaver_threshold, Duration::from_secs(30));
    }
}
```

- [ ] **Step 2: Run the unit tests**

```bash
cargo test -p wc-core lifecycle::idle::tests 2>&1 | tail -10
```

Expected: 3 tests pass.

### Task 14: Screensaver overlay (placeholder)

**Files:**
- Create: `crates/wc-core/src/lifecycle/screensaver.rs`

- [ ] **Step 1: Create the screensaver module**

For Plan 2 the screensaver is a placeholder: it emits a single `tracing::info!` line on show/hide and inserts/removes a marker resource. A real overlay UI lands when bevy-egui is integrated in Plan 5.

Create `crates/wc-core/src/lifecycle/screensaver.rs` with this exact content:

```rust
//! Screensaver overlay shown after sustained idle.
//!
//! In Plan 2 this is a behavioral placeholder: entering the screensaver state
//! logs a message and inserts a marker resource that future systems can read.
//! The actual full-screen overlay UI lands when bevy-egui is integrated in
//! Plan 5 (settings) and uses the same plumbing.

use bevy::prelude::*;

/// Marker resource present iff the screensaver overlay is currently shown.
#[derive(Resource, Default, Debug)]
pub struct ScreensaverActive;

/// `OnEnter(SketchActivity::Screensaver)` handler — inserts the marker.
pub fn show(mut commands: Commands) {
    tracing::info!("screensaver: show");
    commands.insert_resource(ScreensaverActive);
}

/// `OnExit(SketchActivity::Screensaver)` handler — removes the marker.
pub fn hide(mut commands: Commands) {
    tracing::info!("screensaver: hide");
    commands.remove_resource::<ScreensaverActive>();
}
```

(No unit tests yet — this is exercised by the integration tests in Task 16.)

### Task 15: Navigation system

**Files:**
- Create: `crates/wc-core/src/lifecycle/nav.rs`

- [ ] **Step 1: Create the navigation module**

Create `crates/wc-core/src/lifecycle/nav.rs` with this exact content:

```rust
//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle, quit).

use bevy::prelude::*;
use bevy::window::WindowMode;
use leafwing_input_manager::prelude::*;

use super::actions::WaveConductorAction;
use super::state::AppState;

/// Reads `Res<ActionState<WaveConductorAction>>` and translates `just_pressed`
/// events into navigation transitions and side effects.
pub fn handle_navigation_actions(
    actions: Res<ActionState<WaveConductorAction>>,
    current: Res<State<AppState>>,
    mut next: ResMut<NextState<AppState>>,
    mut windows: Query<&mut Window>,
    mut exit: EventWriter<AppExit>,
) {
    use WaveConductorAction as A;

    let mut transition_to: Option<AppState> = None;
    if actions.just_pressed(&A::SelectLine) {
        transition_to = Some(AppState::Line);
    } else if actions.just_pressed(&A::SelectFlame) {
        transition_to = Some(AppState::Flame);
    } else if actions.just_pressed(&A::SelectDots) {
        transition_to = Some(AppState::Dots);
    } else if actions.just_pressed(&A::SelectCymatics) {
        transition_to = Some(AppState::Cymatics);
    } else if actions.just_pressed(&A::SelectWaves) {
        transition_to = Some(AppState::Waves);
    } else if actions.just_pressed(&A::NavigateHome) {
        transition_to = Some(AppState::Home);
    } else if actions.just_pressed(&A::NavigateNext) {
        transition_to = Some(current.get().next_sketch());
    } else if actions.just_pressed(&A::NavigatePrev) {
        transition_to = Some(current.get().prev_sketch());
    }

    if let Some(target) = transition_to {
        if *current.get() != target {
            tracing::info!(?target, "navigate");
            next.set(target);
        }
    }

    if actions.just_pressed(&A::ToggleFullscreen) {
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(?window.mode, "toggle fullscreen");
        }
    }

    if actions.just_pressed(&A::Quit) {
        tracing::info!("quit requested");
        exit.write(AppExit::Success);
    }

    // ToggleVolume and ToggleDevPanel land in Plans 4 and 5 respectively.
    // For now we log so manual testing can verify the action is firing.
    if actions.just_pressed(&A::ToggleVolume) {
        tracing::info!("toggle volume (Plan 4 will handle)");
    }
    if actions.just_pressed(&A::ToggleDevPanel) {
        tracing::info!("toggle dev panel (Plan 5 will handle)");
    }
}
```

(No unit tests here — driving the action state in a unit test requires a full `App`, which is exercised in the integration tests in Task 16.)

If the Bevy 0.18 API for `EventWriter::write` is `send` instead, swap the method name. Same for `WindowMode::BorderlessFullscreen` — newer Bevy versions may have changed the variant signature.

### Task 16: Integration tests for the lifecycle plugin

**Files:**
- Create: `crates/wc-core/tests/lifecycle.rs`

- [ ] **Step 1: Create the integration test file**

Create `crates/wc-core/tests/lifecycle.rs` with this exact content:

```rust
//! Integration tests for `LifecyclePlugin`.
//!
//! Each test stands up a headless `App` with `MinimalPlugins` plus the lifecycle
//! plugin and drives it through realistic sequences using leafwing's action
//! state and manual time advancement.

use std::time::Duration;

use bevy::input::InputPlugin;
use bevy::prelude::*;
use leafwing_input_manager::prelude::*;
use wc_core::lifecycle::{
    actions::WaveConductorAction,
    idle::InteractionTimer,
    state::{AppState, SketchActivity},
    LifecyclePlugin,
};

fn test_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(InputPlugin); // Needed for ButtonInput resources used by leafwing.
    app.add_plugins(LifecyclePlugin);
    app
}

/// Helper: press a leafwing action by mutating the `ActionState` directly,
/// then run an update. Real key events are tricky to drive in a unit test;
/// poking the action state is the leafwing-blessed approach for tests.
fn press_action(app: &mut App, action: WaveConductorAction) {
    {
        let mut state = app
            .world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>();
        state.press(&action);
    }
    app.update();
    {
        let mut state = app
            .world_mut()
            .resource_mut::<ActionState<WaveConductorAction>>();
        state.release(&action);
    }
}

#[test]
fn defaults_to_home_state() {
    let mut app = test_app();
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Home);
}

#[test]
fn select_line_transitions_into_line_state() {
    let mut app = test_app();
    app.update();
    press_action(&mut app, WaveConductorAction::SelectLine);
    // Pending transitions resolve on the next update tick.
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
}

#[test]
fn navigate_home_returns_to_home() {
    let mut app = test_app();
    app.update();
    press_action(&mut app, WaveConductorAction::SelectFlame);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Flame);
    press_action(&mut app, WaveConductorAction::NavigateHome);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Home);
}

#[test]
fn next_and_prev_cycle_through_sketches() {
    let mut app = test_app();
    app.update();
    // Home → next → Line
    press_action(&mut app, WaveConductorAction::NavigateNext);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
    // Line → next → Flame
    press_action(&mut app, WaveConductorAction::NavigateNext);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Flame);
    // Wrap around: 5 nexts from Flame should land back on Flame.
    for _ in 0..5 {
        press_action(&mut app, WaveConductorAction::NavigateNext);
        app.update();
    }
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Flame);
    // Prev from Flame → Line
    press_action(&mut app, WaveConductorAction::NavigatePrev);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
}

#[test]
fn idle_transitions_after_threshold() {
    let mut app = test_app();
    // Configure a short idle threshold so the test is fast.
    app.world_mut()
        .resource_mut::<InteractionTimer>()
        .idle_threshold = Duration::from_millis(50);
    app.world_mut()
        .resource_mut::<InteractionTimer>()
        .screensaver_threshold = Duration::from_millis(50);

    app.update();
    press_action(&mut app, WaveConductorAction::SelectLine);
    app.update();
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Line);
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active,
    );

    // Mark interaction at t=0 then advance the clock past the idle threshold
    // without any input events.
    {
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.mark(Duration::ZERO);
    }
    // Two updates spaced past idle_threshold; Bevy's Time advances based on
    // wall-clock by default, so we manually advance.
    {
        let mut time = app.world_mut().resource_mut::<Time>();
        time.advance_by(Duration::from_millis(80));
    }
    app.update();
    app.update(); // Let the state transition resolve.

    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle,
    );
}
```

- [ ] **Step 2: Run the integration tests**

```bash
cargo test -p wc-core --test lifecycle 2>&1 | tail -15
```

Expected: 5 tests pass.

The `Time::advance_by` API may need adjustment for Bevy 0.18 (it may instead be `time.advance_by(delta)` on a generic `Time<Virtual>` resource, or `Time::set_elapsed`). If the test fails to compile because of `Time` API drift, consult https://docs.rs/bevy/0.18/bevy/time/struct.Time.html for the appropriate method. The intent is: simulate the wall-clock advancing past `idle_threshold` so `advance_activity` observes a long idle.

If leafwing's `ActionState::press`/`release` API differs (e.g., requires a `ButtonInput` event instead of direct state manipulation), the implementer should consult https://docs.rs/leafwing-input-manager for the test-blessed manipulation API. As a fallback, use `state.set_button_data` or whatever leafwing exposes for test fixtures.

### Task 17: Wire `tracing-subscriber` and verify Phase A integration

**Files:**
- Modify: `crates/waveconductor/src/main.rs`

- [ ] **Step 1: Initialize tracing in `main`**

Replace the contents of `crates/waveconductor/src/main.rs` with:

```rust
//! `WaveConductor` v5 binary entry point.
//!
//! Constructs the Bevy [`App`], registers core plugins, and runs the event loop.
//! In Plan 2 this opens a window and exercises the lifecycle plugin (state
//! machine + leafwing keyboard actions). Sketch plugins land in Plan 6 onward.

use bevy::prelude::*;
use tracing_subscriber::EnvFilter;
use wc_core::CorePlugin;
use wc_sketches::SketchesPlugin;

fn main() {
    init_tracing();
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "WaveConductor".into(),
                    resolution: (1280_u32, 720_u32).into(),
                    ..default()
                }),
                ..default()
            }),
            CorePlugin,
            SketchesPlugin,
        ))
        .run();
}

/// Initialize the global tracing subscriber.
///
/// Honors `RUST_LOG` (e.g. `RUST_LOG=info,wc_core=debug`). When unset, defaults
/// to `info` for the application crates so users can see navigation and idle
/// state transitions in the terminal during manual testing.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,waveconductor=info,wc_core=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
```

- [ ] **Step 2: Verify build and tests**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -15
cargo clippy --all-targets --all-features --workspace -- -D warnings 2>&1 | tail -5
cargo fmt --all -- --check
cargo xtask check-secrets 2>&1 | tail -3
```

Expected: build succeeds, all tests pass (was 7 in Plan 1; should be ~16 after this plan: 7 xtask + 1 wc-sketches + 4 state + 1 actions + 3 idle + 5 lifecycle integration = 21 actually). The exact count depends on whether Bevy's MinimalPlugins changes test counts; the floor is "everything green".

- [ ] **Step 3: Commit Phase A**

```bash
git add -A
git status --short
git commit -m "$(cat <<'EOF'
Add lifecycle plugin: AppState, SubStates, idle, leafwing actions

Implements spec §5.2 and the leafwing portion of §5.3. Bevy `States` for
top-level navigation (Home + 5 sketches), `SubStates` for sketch
activity (Active/Idle/Screensaver), `InteractionTimer` driving idle
transitions, and `WaveConductorAction` enum bound to v4's keyboard
hotkey table via leafwing-input-manager.

Screensaver is a logging placeholder; the real overlay UI lands when
bevy-egui is integrated in Plan 5 (settings).

Integration tests cover state transitions, action handling, and idle
threshold advancement.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase B: Manual smoke + branch verification

### Task 18: Manual smoke test (optional but recommended)

**Files:** none

- [ ] **Step 1: Run the binary with logging**

If a graphical environment is available:

```bash
RUST_LOG=info cargo run -p waveconductor
```

Press `1` then `2` then `Escape` and watch the terminal. You should see lines like:

```
INFO navigate target=Line
INFO navigate target=Flame
INFO navigate target=Home
```

If no graphical environment is available (e.g. headless CI), skip this step. The integration tests already cover the logic; the manual smoke test is just confidence-building.

Press `Ctrl+Q` to exit (Quit action). The window should close cleanly.

If the binary panics on startup with a Bevy plugin registration error (e.g. "states resource already inserted" or a SubStates source-attribute mismatch), the implementer should investigate before continuing. Common causes: leafwing's `InputManagerPlugin` requires explicit `<T>` registration; Bevy's `SubStates` requires the `bevy_state` feature enabled in the Bevy dependency (verify it's on by default in 0.18).

### Task 19: Push to remote and verify CI

**Files:** none

- [ ] **Step 1: Push**

```bash
git push origin rewrite/bevy
```

- [ ] **Step 2: Watch CI**

```bash
gh run watch -R madisonrickert/WaveConductor --exit-status 2>&1 | tail -30
```

Expected: all 10 CI jobs green.

If a job fails:

1. Read the failing job's log (`gh run view <id> -R madisonrickert/WaveConductor --log-failed | tail -100`).
2. Reproduce locally if possible.
3. Fix the underlying issue (do NOT loosen lint configuration).
4. Commit and push again.
5. Re-run.

Common causes of CI-only failures:
- Clippy warnings under `--all-features` that didn't appear without it.
- `cargo-deny` flagging a transitive dependency added by leafwing or tracing-subscriber (add a documented advisories.ignore entry like the `paste` case in Plan 1's cargo-deny fix).
- Tests timing out on slow runners (rare; CI has 6-hour default timeout).

### Task 20: Tag the Phase A milestone

**Files:** none

- [ ] **Step 1: Tag and push**

```bash
git tag v5-lifecycle
git push origin v5-lifecycle
```

- [ ] **Step 2: Verify**

```bash
git tag --list 'v5-*'
```

Expected output:
```
v5-foundation
v5-lifecycle
```

---

## Plan complete

At the end of Plan 2, `rewrite/bevy` contains:

- All Plan 1 follow-up housekeeping resolved
- `wc-core/src/lifecycle/` module with state, actions, idle, screensaver, nav submodules
- Bevy `States` (`AppState`) + `SubStates` (`SketchActivity`) wired into `CorePlugin`
- `leafwing-input-manager` set up with v4's full keyboard hotkey map
- A working navigation state machine: pressing 1–5, Z/X, Escape, F11, Shift+D, Ctrl+Q produces tracing logs and state transitions
- Idle and screensaver transitions exercised by tests
- CI green; `v5-lifecycle` tag pushed

**Next plan:** Plan 3 (Input system) adds `HandTrackingPlugin` modeled on `InputPlugin`, the `HandTrackingProvider` trait, the `MockProvider` for tests, and the `Buttonlike` impl for `HandButton` so hand presses flow through leafwing's `ActionState` like mouse and keyboard already do.
