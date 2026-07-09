# Fullscreen and Display Settings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the app boot straight into fullscreen on the deployment monitor (kiosk, no keyboard) and stay there across a TV sleep/wake cycle, without touching `boot_into_attract` (cut, see the index) or the audio-device picker (Plan 04's `AudioSettings`, not this plan's).

**Architecture:** A new `DisplaySettings` resource (`start_fullscreen`, `hide_cursor`, `monitor: String`) is applied by one idempotent system, `apply_display_mode`, that runs once at `Startup` and unconditionally every `Update` frame thereafter. Running it every frame — rather than reacting to a monitor add/remove message — is a deliberate substitution: Bevy 0.19 has no `MonitorAdded` / `MonitorRemoved` message type (verified against vendored source; see Task 2's module doc). Monitors are plain `Monitor` ECS components spawned and despawned once per winit event-loop iteration, so re-deriving the target window mode from `DisplaySettings` plus the live `Monitor` query every frame is cheap, allocation-free, and correct regardless of which frame a monitor actually (dis)appears on. The existing F11 handler (`lifecycle/nav.rs`) stops writing `Window` directly; it now only flips `DisplaySettings.start_fullscreen`, and `apply_display_mode` (ordered after it) is the single writer of `Window::mode` / `CursorOptions::visible`. A second, change-gated system, `sync_available_monitors`, populates a plain `AvailableMonitors(Vec<String>)` resource that Plan 03a's runtime-enumerated widget will read for the `monitor` dropdown; until that widget lands, `monitor` renders as an ordinary text field with identical resolve-by-name semantics.

**Tech Stack:** Rust, Bevy 0.19 (`bevy_window`, `bevy_winit`), the in-house `#[derive(SketchSettings)]` macro (`wc-core-macros`).

**Depends on:** Plan 02 (window-resize invalidation) and Plan 03a (runtime-enumerated setting widget).

- **Plan 02.** Startup fullscreen with no resize handling ships the "framed fullscreen" bug (sketch keeps drawing into the old windowed extent) to every kiosk boot. Land Plan 02 first.
- **Plan 03a.** The `monitor` field needs a dropdown whose options are discovered at runtime — the existing `SettingKind::Enum` (`settings/def.rs:54-60`) only supports a compile-time `&'static [&'static str]` variant list. Plan 03a has shipped (`docs/superpowers/plans/2026-07-09-alpha5-03a-runtime-enum-widget.md`); its contract is now concrete, not assumed. Task 4 consumes it: the `pub trait RuntimeEnumOptionsSource { const OPTIONS_KEY: &'static str; fn options(&self) -> &[String]; }`, the `App` extension `register_runtime_enum_options::<R>()`, the `SettingKind::RuntimeEnum { options_key }` variant, and the `#[setting(ty = RuntimeEnum, options_key = "...")]` derive attribute (a **string-literal key**, compile-time-validated by 03a's macro — not a resource type name). Task 4 does **not** design or build that widget. See Task 4's Interfaces block for the exact symbols.

## Global Constraints

Copied from `AGENTS.md` and Part 1 of `docs/superpowers/plans/2026-07-09-alpha5-program-index.md`. Every task's requirements implicitly include this section.

- **CI gates**, all of which must pass before this plan's work is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings` — **use `--all-targets` when scoping a per-task check too**; `--lib` skips the test target and hides lints in test code.
  - `cargo nextest run --workspace --all-features` (fallback: `cargo test --workspace --all-features` if nextest is absent)
  - `cargo test --doc --workspace` (nextest does not run doctests)
  - `cargo doc --no-deps --workspace --document-private-items` (CI runs with `RUSTDOCFLAGS="-D warnings"`; **do not** add `--all-features` when reproducing this locally — it surfaces unrelated feature-gated noise). A **public** item's rustdoc must never intra-doc-link to a `pub(crate)`/private item — demote to a plain code span (`` [`Foo`] `` → `` `Foo` ``); this broke Plan 01 twice.
  - `cargo deny check`
  - `cargo xtask check-secrets` — scans everything except `vendor/`, `target/`, `.git/`, and `docs/superpowers/`'s dated planning archive; no developer home paths, emails, or secret prefixes.
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)] mod tests`.** `unwrap_used`, `expect_used`, `panic`, and `as_conversions` are all denied. Where a test genuinely needs `.expect(...)` for a clear failure message, add a scoped `#[allow(clippy::expect_used, reason = "...")]` on the `mod tests` block rather than avoiding it awkwardly — see Task 1 for the pattern. `assert_eq!(x.is_some(), true)` → use `assert!(x.is_some())`. `0..(N + 1)` → use `0..=N`.
- **A type or function with no non-test caller is dead code** on the lib target (compiled without `cfg(test)`) and fails `-D warnings`. When this plan introduces types before their consumer (Task 1's `DisplaySettings` / `AvailableMonitors` / pure functions have no production caller until Task 2 wires them in), it carries a transient `#![allow(dead_code)]` with an explicit removal step in the task that adds the first real caller. Do not skip the removal step.
- **No `unwrap()` / `expect()` in non-test code.** No `as` casts where `From` / `TryFrom` would work. `///` rustdoc on every public item; `//!` module doc on every new file. Public API at the top of a file, private helpers at the bottom, tests in a `#[cfg(test)] mod tests` block at the footer. One concept per file.
- **Never allocate in a hot path** — per-frame Bevy systems included. `apply_display_mode` is written to allocate nothing on any frame where nothing changed (comparisons over `Copy` types only); `sync_available_monitors` is the one system in this plan that touches heap (`Vec<String>` / cloned `String`s) and is therefore gated to run its allocating body only on a frame where a `Monitor` was actually added or removed.
- **No GPU tests in CI.** Everything in `crates/wc-core/tests/ui_blur.rs` is `#[ignore]`d because `DefaultPlugins` needs the macOS main thread; `cargo xtask capture` returns all-black frames when the app window isn't foregrounded, so an agent cannot use it to verify windowing/fullscreen behaviour. **Every visual/windowing behaviour in this plan has an explicit human verification step naming `cargo rund`.** The logic that *can* be tested without a GPU or a live `App` (default resolution, monitor-name resolution, the debug/release default, the window-mode/cursor computation) is factored into pure functions operating on plain data (`Window`, `CursorOptions`, `WindowMode`, `MonitorSelection` are ordinary structs/enums — constructing them in a unit test needs no GPU, no winit, and no `App`) and is unit-tested for real.
- **Commit messages:** write the message to a file, then `git commit -F <file>`. Never `-m` — backticks in a `-m` string are shell-substituted by zsh and silently eat words. **Never `git add -A`** — stage named paths only, then confirm with `git show --stat HEAD`.
- **Branch:** this work lands on `windows-remediation` (or whatever branch the alpha.5 program is using at execution time), after Plan 02 and Plan 03a.
- **Never put `bevy/dynamic_linking` in a manifest `[features]` table.** Manual smoke tests use `cargo rund`.

---

## Task 1: `DisplaySettings`, `AvailableMonitors`, and the pure resolution logic

**Files:**
- Create: `crates/wc-core/src/settings/panel_user/display.rs`
- Modify: `crates/wc-core/src/settings/panel_user/mod.rs:60` (add `pub(super) mod display;` before the existing `mod dock;`)

**Interfaces:**
- Consumes: nothing outside this crate. `wc_core_macros::SketchSettings` (existing derive), `bevy::window::{MonitorSelection, WindowMode}` (existing Bevy types, both plain data — no GPU/winit/App needed to construct or compare them).
- Produces:
  - `pub(crate) struct DisplaySettings { pub start_fullscreen: bool, pub hide_cursor: bool, pub monitor: String }` — a `SketchSettings` resource, `storage_key = "display"`.
  - `pub(crate) struct AvailableMonitors(pub(crate) Vec<String>)` — plain `Resource`, not a `SketchSettings` (not persisted; rebuilt from live OS state).
  - `pub(crate) struct DisplayModeTarget { pub(crate) mode: WindowMode, pub(crate) cursor_visible: bool }`
  - `pub(crate) fn compute_display_mode<'a>(settings: &DisplaySettings, live_monitors: impl IntoIterator<Item = (Entity, Option<&'a str>)>) -> DisplayModeTarget`
  - `resolve_monitor_selection` and the `default_*` functions stay module-private; only used from within this file (by `compute_display_mode` and by the derive-generated `Default` impl, both in the same module).
- **No non-test caller yet, and not re-exported yet either.** Task 2 wires `compute_display_mode` and `AvailableMonitors` into real systems, and is where the `pub(crate) use` re-export into `settings/mod.rs` is added — adding that re-export in this task, before anything outside `display.rs` consumes it, would itself be an unused `pub(crate)` import (narrower than `pub use`, so rustc *can* prove it unused within the crate, unlike a fully-`pub` re-export). This task's `#![allow(dead_code)]` (see Step 4) is transient; Task 2, Step 6 removes it.

This task has **no dependency on Plan 03a** — `monitor` is declared here as an ordinary `ty = Text` field. Task 4 is the only place this plan touches the 03a widget.

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/settings/panel_user/display.rs` containing only the test module, so it fails to compile against the missing types:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    fn entity(raw: u32) -> Entity {
        Entity::from_raw_u32(raw).expect("small literal is a valid non-max raw id")
    }

    // --- default_start_fullscreen_for ---

    #[test]
    fn start_fullscreen_defaults_to_false_under_debug_assertions_so_cargo_rund_stays_sane() {
        assert!(!default_start_fullscreen_for(true));
    }

    #[test]
    fn start_fullscreen_defaults_to_true_in_release_so_a_kiosk_boots_fullscreen() {
        assert!(default_start_fullscreen_for(false));
    }

    // --- resolve_monitor_selection ---

    #[test]
    fn empty_saved_name_resolves_to_current_regardless_of_live_monitors() {
        let live = [(entity(1), Some("DELL U2720Q"))];
        assert_eq!(resolve_monitor_selection("", live), MonitorSelection::Current);
    }

    #[test]
    fn a_saved_name_matching_a_live_monitor_resolves_to_its_entity() {
        let target = entity(2);
        let live = [(entity(1), Some("Built-in Display")), (target, Some("LG TV"))];
        assert_eq!(
            resolve_monitor_selection("LG TV", live),
            MonitorSelection::Entity(target)
        );
    }

    #[test]
    fn a_saved_name_with_no_live_match_falls_back_to_current() {
        // The caller (compute_display_mode / apply_display_mode) never mutates
        // `saved_name` in this case — the type signature makes that
        // structurally true: `&str` in, no mutation possible. An HDMI TV
        // that is merely asleep must not lose its saved binding.
        let live = [(entity(1), Some("Built-in Display"))];
        assert_eq!(
            resolve_monitor_selection("LG TV (asleep)", live),
            MonitorSelection::Current
        );
    }

    #[test]
    fn a_saved_name_resolves_against_an_empty_monitor_list_to_current() {
        // Covers the Startup-vs-create_monitors race: at boot the ECS may not
        // have any Monitor entities yet. Falling back here is what makes that
        // race harmless — see the module doc on compute_display_mode.
        let live: [(Entity, Option<&str>); 0] = [];
        assert_eq!(resolve_monitor_selection("LG TV", live), MonitorSelection::Current);
    }

    #[test]
    fn an_unnamed_live_monitor_never_matches_a_non_empty_saved_name() {
        let live = [(entity(1), None)];
        assert_eq!(resolve_monitor_selection("LG TV", live), MonitorSelection::Current);
    }

    // --- compute_display_mode ---

    #[test]
    fn windowed_when_start_fullscreen_is_false_regardless_of_monitor() {
        let settings = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [(entity(1), Some("LG TV"))];
        let target = compute_display_mode(&settings, live);
        assert_eq!(target.mode, WindowMode::Windowed);
    }

    #[test]
    fn fullscreen_on_current_when_monitor_is_unset() {
        let settings = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: false,
            monitor: String::new(),
        };
        let target = compute_display_mode(&settings, []);
        assert_eq!(
            target.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        );
    }

    #[test]
    fn fullscreen_on_the_named_monitor_when_it_resolves() {
        let target_entity = entity(3);
        let settings = DisplaySettings {
            start_fullscreen: true,
            hide_cursor: false,
            monitor: "LG TV".to_string(),
        };
        let live = [(entity(1), Some("Built-in Display")), (target_entity, Some("LG TV"))];
        let target = compute_display_mode(&settings, live);
        assert_eq!(
            target.mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Entity(target_entity))
        );
    }

    #[test]
    fn cursor_visible_is_the_negation_of_hide_cursor() {
        let hidden = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: true,
            monitor: String::new(),
        };
        let shown = DisplaySettings {
            start_fullscreen: false,
            hide_cursor: false,
            monitor: String::new(),
        };
        assert!(!compute_display_mode(&hidden, []).cursor_visible);
        assert!(compute_display_mode(&shown, []).cursor_visible);
    }

    // --- struct plumbing ---

    #[test]
    fn display_settings_default_matches_the_debug_build_default() {
        // `cargo test` always compiles under debug_assertions, so this
        // exercises the same branch as `cargo rund`; the release branch is
        // covered directly by `start_fullscreen_defaults_to_true_in_release...`
        // above via the parameterised helper.
        let settings = DisplaySettings::default();
        assert!(!settings.start_fullscreen);
        assert!(settings.hide_cursor);
        assert_eq!(settings.monitor, String::new());
    }

    #[test]
    fn available_monitors_defaults_empty() {
        assert!(AvailableMonitors::default().0.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib settings::panel_user::display 2>&1 | head -30`

Expected: FAIL to compile — `cannot find type DisplaySettings in this scope` (and similarly for `AvailableMonitors`, `resolve_monitor_selection`, `compute_display_mode`, `default_start_fullscreen_for`).

- [ ] **Step 3: Register the module**

In `crates/wc-core/src/settings/panel_user/mod.rs`, the module declarations currently read (line 60 onward):

```rust
mod dock;
mod fields;
mod provider_status;
#[cfg(feature = "templates")]
mod template_picker;
mod widgets;
```

Change to:

```rust
pub(super) mod display;
mod dock;
mod fields;
mod provider_status;
#[cfg(feature = "templates")]
mod template_picker;
mod widgets;
```

`pub(super)` (not plain `mod`) is required: `display`'s items need to reach `settings/mod.rs` (the parent of `panel_user`), and Rust's default module privacy only reaches descendants of the declaring module, not ancestors. Every other `panel_user` submodule stays plain `mod` because nothing outside `panel_user` needs them directly.

- [ ] **Step 4: Write the implementation**

Prepend to `crates/wc-core/src/settings/panel_user/display.rs`, above the test module:

```rust
//! Core (not per-sketch) display settings: startup fullscreen, cursor
//! visibility, and monitor selection.
//!
//! ## Why this file lives under `settings/panel_user/`
//!
//! Every other file in this directory is shared panel-*rendering*
//! infrastructure (`dock`, `fields`, `widgets`, `provider_status`), not a
//! settings struct's home — those normally live directly under `settings/`
//! (`hand_tracking.rs`) or beside the domain they configure
//! (`lifecycle/screensaver/settings.rs`). This placement is a locked design
//! decision (`docs/superpowers/specs/2026-07-08-windows-remediation-design.md`,
//! Workstream 4), not this plan's choice, kept here as written rather than
//! relitigated. The *systems* that apply these settings — where the domain
//! logic actually lives — are in `crate::lifecycle::display`, matching the
//! rest of `lifecycle/`.
//!
//! ## `monitor: String`, not `Option<String>`
//!
//! The design doc and the alpha.5 program index describe `monitor:
//! Option<String>` with a fallback to `MonitorSelection::Current`. Plan
//! 03a's widget contract (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`,
//! Plan 03a entry) stores a plain `String`, matching every other `Text`-kind
//! setting (no persistence-format change). This plan reconciles the two:
//! `monitor: String`, where an **empty string** is the `None` case and always
//! resolves to [`MonitorSelection::Current`] — see [`resolve_monitor_selection`].
//! The persisted semantics (`Some(name)` / `None` / unresolvable-name
//! fallback) are unchanged; only the Rust type is.

// Transient. `DisplaySettings`, `AvailableMonitors`, `compute_display_mode`,
// and `resolve_monitor_selection` have no non-test caller until Task 2 wires
// them into `crate::lifecycle::display`'s systems, so the lib target
// (compiled without `cfg(test)`) sees them as dead code and `clippy -D
// warnings` fails. Task 2, Step 6 removes this attribute and verifies clippy
// stays clean without it.
//
// Inner attributes (`#![...]`) must precede every other item in the module,
// `use` declarations included — this has to sit above them, not below, or
// rustc rejects it.
#![allow(dead_code)]

use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

/// Startup fullscreen, cursor visibility, and monitor selection.
///
/// Applied by `crate::lifecycle::display::apply_display_mode`, which runs
/// once at `Startup` and unconditionally every `Update` frame thereafter —
/// see that module's doc for why "every frame" is the mechanism that stands
/// in for a `MonitorAdded` / `MonitorRemoved` message Bevy 0.19 does not have.
/// The F11 keybind (`lifecycle::nav::handle_navigation_actions`) writes only
/// `start_fullscreen`; it does not touch `Window` directly.
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "display")]
pub(crate) struct DisplaySettings {
    /// Whether the app claims the whole target monitor at startup.
    ///
    /// Default `true` in release (a kiosk build with no attached keyboard has
    /// no other way to reach fullscreen), `false` under `debug_assertions` so
    /// `cargo rund` does not swallow the dev window on every relaunch. See
    /// [`default_start_fullscreen_for`] for the testable branch logic.
    #[setting(
        default = default_start_fullscreen(),
        ty = Boolean,
        section = "Display",
        category = User,
        label = "Start fullscreen"
    )]
    #[serde(default = "default_start_fullscreen")]
    pub start_fullscreen: bool,

    /// Whether to hide the OS mouse cursor over the window.
    ///
    /// Default `true` — kiosk-first, matching `ScreensaverSettings`'s
    /// `keep_display_awake` precedent (default on; an unattended install has
    /// no reason to show a cursor). Toggle off for dev sessions where a
    /// visible cursor is convenient.
    #[setting(
        default = true,
        ty = Boolean,
        section = "Display",
        category = User,
        label = "Hide cursor"
    )]
    #[serde(default = "default_hide_cursor")]
    pub hide_cursor: bool,

    /// Monitor to target when `start_fullscreen` is true, persisted by
    /// **name** (not by winit's per-boot enumeration index, which is not
    /// stable across reboots or monitor topology changes). Empty string means
    /// "no preference" and resolves to [`MonitorSelection::Current`] via
    /// [`resolve_monitor_selection`].
    ///
    /// Renders as a plain text field until Task 4 upgrades it to Plan 03a's
    /// runtime-enumerated dropdown. The stored value and its resolution
    /// semantics are identical before and after that upgrade; only the widget
    /// changes.
    #[setting(
        default = String::new(),
        ty = Text,
        section = "Display",
        category = User,
        label = "Monitor"
    )]
    #[serde(default)]
    pub monitor: String,
}

/// Monitor names currently reported by the OS.
///
/// Refreshed by `crate::lifecycle::display::sync_available_monitors`
/// whenever a `Monitor` ECS component is added or removed (not every frame —
/// see that system's doc for why). A plain `Resource` newtype here, with **no**
/// dependency on Plan 03a: Task 4 adds the `impl RuntimeEnumOptionsSource`
/// (keyed `"monitors"`) and the `register_runtime_enum_options` call that let
/// Plan 03a's runtime-enumerated widget populate the `monitor` field's
/// dropdown from it. Keeping the impl out of this task is what keeps Tasks 1-3
/// buildable before 03a lands.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub(crate) struct AvailableMonitors(pub(crate) Vec<String>);

/// The window mode and cursor visibility implied by a [`DisplaySettings`]
/// value, computed against a snapshot of live monitors.
///
/// A named struct rather than a `(WindowMode, bool)` tuple, per AGENTS.md's
/// preference for named fields once a type carries more than one
/// semantically meaningful value — `target.cursor_visible` reads at the call
/// site; `target.1` does not.
pub(crate) struct DisplayModeTarget {
    /// The window mode to apply (`Windowed` or `BorderlessFullscreen(_)`).
    pub(crate) mode: WindowMode,
    /// Whether the OS cursor should be visible (`!settings.hide_cursor`).
    pub(crate) cursor_visible: bool,
}

/// Pure computation of the window mode and cursor visibility implied by
/// `settings`, given the monitors currently known to the ECS.
///
/// Shared by `apply_display_mode`'s `Startup` run (booting the kiosk) and its
/// `Update` run (every frame thereafter), so a settings-panel edit, an F11
/// toggle, and a monitor add/remove all re-derive the same target instead of
/// three code paths that can disagree about what "fullscreen" currently
/// means.
///
/// Takes an iterator rather than a slice so a live `Query` never has to
/// collect into a `Vec` first (AGENTS.md's "never allocate in a hot path" —
/// `resolve_monitor_selection`'s `find` short-circuits on the first match, so
/// nothing downstream needs the full list materialised).
pub(crate) fn compute_display_mode<'a>(
    settings: &DisplaySettings,
    live_monitors: impl IntoIterator<Item = (Entity, Option<&'a str>)>,
) -> DisplayModeTarget {
    let mode = if settings.start_fullscreen {
        WindowMode::BorderlessFullscreen(resolve_monitor_selection(
            &settings.monitor,
            live_monitors,
        ))
    } else {
        WindowMode::Windowed
    };
    DisplayModeTarget {
        mode,
        cursor_visible: !settings.hide_cursor,
    }
}

/// Resolve a persisted monitor name to a [`MonitorSelection`] against the
/// monitors currently known to the ECS.
///
/// - An empty `saved_name` (the field's default) means "no preference":
///   always [`MonitorSelection::Current`].
/// - A non-empty name matching a live monitor's `Some(name)` resolves to
///   that monitor's `Entity`.
/// - A non-empty name with no live match — the monitor is asleep, unplugged,
///   or winit has not enumerated any monitors yet (this can run at
///   `Startup`, which may race `bevy_winit`'s monitor sync) — falls back to
///   [`MonitorSelection::Current`]. The caller never rewrites `saved_name` in
///   this case; the `&str` parameter makes that structurally true. An HDMI TV
///   that is merely asleep must not lose its saved binding.
fn resolve_monitor_selection<'a>(
    saved_name: &str,
    live_monitors: impl IntoIterator<Item = (Entity, Option<&'a str>)>,
) -> MonitorSelection {
    if saved_name.is_empty() {
        return MonitorSelection::Current;
    }
    live_monitors
        .into_iter()
        .find(|(_, name)| *name == Some(saved_name))
        .map_or(MonitorSelection::Current, |(entity, _)| {
            MonitorSelection::Entity(entity)
        })
}

/// Serde fallback and `#[setting(default = ...)]` value for `start_fullscreen`.
///
/// Delegates to the pure, fully unit-tested [`default_start_fullscreen_for`]
/// so both branches of the `cfg!` are exercised by tests without needing to
/// recompile under a different profile — `cargo test` / `cargo nextest`
/// always run under `debug_assertions`, so a direct `cfg!(debug_assertions)`
/// call in a test would only ever cover one branch.
fn default_start_fullscreen() -> bool {
    default_start_fullscreen_for(cfg!(debug_assertions))
}

/// Pure branch behind [`default_start_fullscreen`]. `cargo rund` (dev) must
/// not default to fullscreen, or every dev relaunch would swallow the window;
/// a release/kiosk build must, or a field tester with no keyboard has no way
/// to find F11.
fn default_start_fullscreen_for(is_debug_build: bool) -> bool {
    !is_debug_build
}

/// Serde fallback and `#[setting(default = ...)]` value for `hide_cursor`.
/// Kiosk-first: the display stays cursor-free by default.
fn default_hide_cursor() -> bool {
    true
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib settings::panel_user::display`

Expected: PASS, 12 tests.

- [ ] **Step 6: Run the scoped gate and commit**

The workspace-wide clippy gate is deliberately not run here; the controller runs it between tasks. `DisplaySettings` and `AvailableMonitors` are not yet re-exported from `settings/mod.rs` — Task 2 adds that re-export in the same step that introduces the first consumer, so there is never a window where the re-export itself is an unused `pub(crate) use` (see this task's Interfaces block).

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib settings::panel_user::display
git add crates/wc-core/src/settings/panel_user/display.rs crates/wc-core/src/settings/panel_user/mod.rs
git commit -F <message file>
```

Message:

```
feat(settings): add DisplaySettings, AvailableMonitors, and pure display-mode resolution

New core (not per-sketch) settings section: start_fullscreen (default true
in release, false under debug_assertions), hide_cursor (default true), and
monitor (persisted by name, empty = no preference). compute_display_mode
and resolve_monitor_selection are pure functions over plain Window/
MonitorSelection data, fully unit-tested without a GPU or a live App.

No production caller yet -- crate::lifecycle::display (Task 2) wires these
in. Carries a transient #![allow(dead_code)] removed there.
```

---

## Task 2: `apply_display_mode` and `sync_available_monitors`

**Files:**
- Create: `crates/wc-core/src/lifecycle/display.rs`
- Modify: `crates/wc-core/src/lifecycle/mod.rs:20-21` (add `pub mod display;`), and inside `LifecyclePlugin::build` (add `app.add_plugins(display::DisplayPlugin);`)
- Modify: `crates/wc-core/src/settings/mod.rs:38` (add the `pub(crate) use panel_user::display::{...}` re-export — see Step 5)

**Interfaces:**
- Consumes: `crate::settings::{DisplaySettings, AvailableMonitors, compute_display_mode}` (Task 1; `DisplayModeTarget` is returned by `compute_display_mode` but never named explicitly here — type inference is enough, so it is not re-exported), `RegisterSketchSettingsExt::register_sketch_settings` (existing, `settings/registry.rs:169-175`), `bevy::window::{CursorOptions, Monitor, WindowMode}`, `crate::lifecycle::nav::handle_navigation_actions` (existing `pub fn`, used only as an ordering anchor — Task 3 changes its body, not its signature or visibility).
- Produces:
  - `pub struct DisplayPlugin;` (`impl Plugin for DisplayPlugin`)
  - `pub(crate) fn apply_display_mode(settings: Res<'_, DisplaySettings>, monitors: Query<'_, '_, (Entity, &Monitor)>, windows: Query<'_, '_, (&mut Window, &mut CursorOptions)>)`
  - `pub(crate) fn sync_available_monitors(available: ResMut<'_, AvailableMonitors>, monitors: Query<'_, '_, &Monitor>, added: Query<'_, '_, (), Added<Monitor>>, removed: RemovedComponents<'_, '_, Monitor>)`

This task removes Task 1's transient `#![allow(dead_code)]`.

- [ ] **Step 1: Write the failing test**

`apply_display_mode` and `sync_available_monitors` are themselves thin ECS glue (Bevy systems, not pure functions), so — consistent with "no GPU tests in CI" — they are not unit-tested directly; Task 1 already covers the logic they call. This step instead locks in that the module compiles and `DisplayPlugin::build` runs without panicking, mirroring the existing `core_plugin_builds_without_panicking` pattern in `crates/wc-core/src/lib.rs` **exactly**, including that pattern's choice not to call `app.update()`.

`app.update()` is deliberately **not** called here. `apply_display_mode` is ordered `.after(crate::lifecycle::nav::handle_navigation_actions)`, a system this isolated test never adds (it adds only `DisplayPlugin`, not the whole `LifecyclePlugin`) — running the schedule would be exercising an ordering constraint against a system this test intentionally leaves out, for no coverage this plan's other tests don't already provide more cheaply. Whether `apply_display_mode` panics when it actually runs is exactly what Task 3's manual `cargo rund` step verifies; a construction-only test here (matching every other `CorePlugin`/sub-plugin test in this file) is the right amount of automated coverage for ECS glue that calls already-tested pure functions.

Create `crates/wc-core/src/lifecycle/display.rs` containing only the test module:

```rust
#[cfg(test)]
mod tests {
    use bevy::prelude::*;

    use super::*;

    #[test]
    fn display_plugin_registers_its_resources_without_panicking() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(DisplayPlugin);
        assert!(app.world().contains_resource::<crate::settings::DisplaySettings>());
        assert!(app.world().contains_resource::<crate::settings::AvailableMonitors>());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib lifecycle::display 2>&1 | head -30`

Expected: FAIL to compile — `cannot find type DisplayPlugin in this scope` (and, before Step 3 adds the module declaration, `cannot find module display in crate::lifecycle` at the `mod.rs` level too). Add the module declaration (Step 3) first, then confirm the remaining failure is specifically the missing `DisplayPlugin` type, not a missing module path.

- [ ] **Step 3: Declare the module**

In `crates/wc-core/src/lifecycle/mod.rs`, the module list currently reads (lines 19-26):

```rust
pub mod action_map;
pub mod actions;
pub mod idle;
pub mod nav;
pub mod reload;
pub mod screensaver;
pub mod state;
pub mod thermal;
```

Change to:

```rust
pub mod action_map;
pub mod actions;
pub mod display;
pub mod idle;
pub mod nav;
pub mod reload;
pub mod screensaver;
pub mod state;
pub mod thermal;
```

- [ ] **Step 4: Write the implementation**

Prepend to `crates/wc-core/src/lifecycle/display.rs`, above the test module:

```rust
//! Applies `DisplaySettings` to the primary window: startup fullscreen,
//! monitor selection, and cursor visibility.
//!
//! ## There is no `MonitorAdded` / `MonitorRemoved` message in Bevy 0.19
//!
//! The design doc and the alpha.5 program index both describe re-asserting
//! fullscreen "on `MonitorAdded` / `MonitorRemoved`". Verified against the
//! vendored `bevy_window-0.19.0` and `bevy_winit-0.19.0` source (2026-07-09):
//! **no such message type exists.** Monitors are plain [`Monitor`] ECS
//! components. `bevy_winit::system::create_monitors`
//! (`bevy_winit-0.19.0/src/system.rs:177-234`) `commands.spawn(Monitor {
//! .. })`s a new one, and `commands.entity(*entity).despawn()`s a
//! disappeared one, once per winit event-loop iteration, from
//! `WinitAppRunnerState::about_to_wait`
//! (`bevy_winit-0.19.0/src/state.rs:459-462`) — there is no dedicated
//! message, only component add/remove.
//!
//! Rather than reconstruct add/remove *events* from `Added<Monitor>` /
//! `RemovedComponents<Monitor>` on the mode-applying system (and risk a
//! frame where the reader is consumed by the wrong system, or a race between
//! `Startup` and the first `create_monitors` call), [`apply_display_mode`]
//! runs **unconditionally every `Update` frame** and idempotently re-derives
//! the window's target mode from `DisplaySettings` plus a fresh `Monitor`
//! query via `crate::settings::compute_display_mode`. This is cheap — a
//! handful of entity comparisons, no allocation, no-op when nothing changed
//! — and correct regardless of which frame a monitor actually (dis)appears
//! on: it re-asserts by construction, every frame, rather than by reacting to
//! an event that does not exist. It is also correct at boot even if
//! `Startup` runs before `create_monitors`'s first pass: the query is simply
//! empty that frame, `resolve_monitor_selection` falls back to
//! `MonitorSelection::Current`, and the very next `Update` frame converges on
//! the named monitor once it exists.
//!
//! [`sync_available_monitors`] is the one system here that allocates
//! (`AvailableMonitors` owns a `Vec<String>`), so — unlike
//! `apply_display_mode` — it *is* gated on an actual add/remove signal,
//! read once and fully drained so the same event cannot re-trigger it on a
//! later frame.

use bevy::prelude::*;
use bevy::window::{CursorOptions, Monitor, WindowMode};

use crate::settings::{compute_display_mode, AvailableMonitors, DisplaySettings};
use crate::settings::RegisterSketchSettingsExt;

/// Plugin: registers [`DisplaySettings`], initialises [`AvailableMonitors`],
/// and wires [`apply_display_mode`] / [`sync_available_monitors`].
///
/// Registered by [`crate::lifecycle::LifecyclePlugin`].
pub struct DisplayPlugin;

impl Plugin for DisplayPlugin {
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<DisplaySettings>();
        app.init_resource::<AvailableMonitors>();
        // Boot-time apply. May race bevy_winit's first `create_monitors`
        // pass (see the module doc); harmless, because the Update-scheduled
        // copy below converges on the next frame regardless.
        app.add_systems(Startup, apply_display_mode);
        app.add_systems(
            Update,
            (
                sync_available_monitors,
                // Ordered after the F11 handler so a toggle takes effect the
                // same frame it is pressed, not one frame later.
                apply_display_mode.after(crate::lifecycle::nav::handle_navigation_actions),
            ),
        );
    }
}

/// Idempotently apply [`DisplaySettings`] to every window.
///
/// Runs at `Startup` and unconditionally every `Update` frame — see the
/// module doc for why "every frame" stands in for a monitor-topology message
/// Bevy 0.19 does not have. Writes `Window::mode` / `CursorOptions::visible`
/// only when the computed value actually differs from the current one, so an
/// unchanged frame does not spuriously mark either component "changed" for
/// downstream change-detection consumers (e.g. Plan 02's resize listeners).
pub(crate) fn apply_display_mode(
    settings: Res<'_, DisplaySettings>,
    monitors: Query<'_, '_, (Entity, &Monitor)>,
    mut windows: Query<'_, '_, (&mut Window, &mut CursorOptions)>,
) {
    let target = compute_display_mode(
        &settings,
        monitors.iter().map(|(entity, monitor)| (entity, monitor.name.as_deref())),
    );
    for (mut window, mut cursor) in &mut windows {
        if window.mode != target.mode {
            window.mode = target.mode;
        }
        if cursor.visible != target.cursor_visible {
            cursor.visible = target.cursor_visible;
        }
    }
}

/// Refresh [`AvailableMonitors`] from the live `Monitor` set, but only on a
/// frame where a monitor was actually added or removed.
///
/// `removed.read().count()` fully drains the `RemovedComponents` reader, so
/// a removal is consumed exactly once and cannot re-trigger this system on a
/// later frame it never happened on (unlike calling `.is_empty()`, which
/// would peek without advancing the cursor). `Added<Monitor>` needs no
/// equivalent draining — Bevy scopes it to "added since this query's last
/// run" automatically.
pub(crate) fn sync_available_monitors(
    mut available: ResMut<'_, AvailableMonitors>,
    monitors: Query<'_, '_, &Monitor>,
    added: Query<'_, '_, (), Added<Monitor>>,
    mut removed: RemovedComponents<'_, '_, Monitor>,
) {
    let any_removed = removed.read().count() > 0;
    if added.is_empty() && !any_removed {
        return;
    }
    available.0.clear();
    available.0.extend(monitors.iter().filter_map(|m| m.name.clone()));
}
```

- [ ] **Step 5: Re-export `DisplaySettings` / `AvailableMonitors` / `compute_display_mode` at crate visibility**

`lifecycle/display.rs`'s `use crate::settings::{compute_display_mode, AvailableMonitors, DisplaySettings};` (Step 4) needs these visible outside `settings::panel_user`. In `crates/wc-core/src/settings/mod.rs`, the module declares `mod panel_user;` at line 38 and its `pub use` block runs lines 40-48. Add, immediately after `mod panel_user;`:

```rust
mod panel_user;

// `DisplaySettings` / `AvailableMonitors` / `compute_display_mode` are
// crate-internal (consumed by `crate::lifecycle::display`, not by sketch
// crates or the binary), hence `pub(crate)` rather than the `pub use` used
// for the fully public settings types below.
pub(crate) use panel_user::display::{compute_display_mode, AvailableMonitors, DisplaySettings};
```

- [ ] **Step 6: Wire `DisplayPlugin` into `LifecyclePlugin` and remove Task 1's transient allow**

In `crates/wc-core/src/lifecycle/mod.rs`, inside `LifecyclePlugin::build`, the existing tail reads:

```rust
        // Screensaver / attract-mode framework (Plan 11.8, Seam 2). Owns the
        // `in_screensaver` run-condition, the `ScreensaverSettings` resource,
        // the instruction overlay, and the per-tier present-rate throttle.
        app.add_plugins(screensaver::ScreensaverPlugin);
    }
}
```

Change to:

```rust
        // Screensaver / attract-mode framework (Plan 11.8, Seam 2). Owns the
        // `in_screensaver` run-condition, the `ScreensaverSettings` resource,
        // the instruction overlay, and the per-tier present-rate throttle.
        app.add_plugins(screensaver::ScreensaverPlugin);

        // Startup fullscreen, cursor visibility, and monitor selection (Plan
        // 03, alpha.5). Applies DisplaySettings at boot and re-asserts it
        // every frame — see crate::lifecycle::display's module doc for why
        // "every frame" replaces the MonitorAdded/MonitorRemoved message
        // Bevy 0.19 does not have.
        app.add_plugins(display::DisplayPlugin);
    }
}
```

Now delete the transient attribute block from `crates/wc-core/src/settings/panel_user/display.rs` — these nine lines just above `use bevy::prelude::*;`:

```rust
// Transient. `DisplaySettings`, `AvailableMonitors`, `compute_display_mode`,
// and `resolve_monitor_selection` have no non-test caller until Task 2 wires
// them into `crate::lifecycle::display`'s systems, so the lib target
// (compiled without `cfg(test)`) sees them as dead code and `clippy -D
// warnings` fails. Task 2, Step 6 removes this attribute and verifies clippy
// stays clean without it.
//
// Inner attributes (`#![...]`) must precede every other item in the module,
// `use` declarations included — this has to sit above them, not below, or
// rustc rejects it.
#![allow(dead_code)]
```

If clippy then reports `dead_code` on anything in that file, the wiring in Step 4 or Step 5 is incomplete — fix the wiring, do not restore the attribute.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib lifecycle::display`

Expected: PASS, 1 test.

- [ ] **Step 8: Run the scoped gate and commit**

```bash
rg -n "allow\(dead_code\)" crates/wc-core/src/settings/panel_user/display.rs   # expect: no matches
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib lifecycle
git add crates/wc-core/src/lifecycle/display.rs crates/wc-core/src/lifecycle/mod.rs crates/wc-core/src/settings/panel_user/display.rs crates/wc-core/src/settings/mod.rs
git commit -F <message file>
```

Message:

```
feat(lifecycle): apply DisplaySettings at boot and re-assert every frame

DisplayPlugin registers DisplaySettings, initialises AvailableMonitors, and
wires apply_display_mode (Startup + every Update frame) and
sync_available_monitors (gated on an actual Monitor add/remove).

Bevy 0.19 has no MonitorAdded/MonitorRemoved message (verified against
vendored source) -- monitors are plain ECS components spawned/despawned
once per winit event-loop iteration. Running apply_display_mode
unconditionally every frame is the substitute: cheap, allocation-free on
an unchanged frame, and correct regardless of which frame a monitor
actually (dis)appears on.
```

---

## Task 3: F11 writes `start_fullscreen` instead of `Window` directly

**Files:**
- Modify: `crates/wc-core/src/lifecycle/nav.rs:1-7` (imports), `:19-25` (function signature), `:62-71` (fullscreen block)

**Interfaces:**
- Consumes: `crate::settings::DisplaySettings` (Task 1), `crate::lifecycle::display::apply_display_mode`'s `Update`-schedule ordering (Task 2 — `apply_display_mode` is already ordered `.after(handle_navigation_actions)`, so no change is needed on that side).
- Produces: `handle_navigation_actions`'s signature changes (one `Query` param replaced by one `ResMut` param); its behavior for every other action is unchanged.

- [ ] **Step 1: Confirm the current state (no test to write — this is a refactor of existing, already-tested navigation code)**

`handle_navigation_actions` has no dedicated unit tests today (it is exercised only via `cargo rund`, per the file's existing doc comments), so there is no failing-test step here. Verify the current fullscreen block before editing:

Run: `sed -n '1,10p;62,71p' crates/wc-core/src/lifecycle/nav.rs`

Expected: imports include `use bevy::window::WindowMode;` at line 5, and lines 62-71 read:

```rust
    if fullscreen {
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(mode = ?window.mode, "toggle fullscreen");
        }
    }
```

- [ ] **Step 2: Remove the now-unused import**

In `crates/wc-core/src/lifecycle/nav.rs`, delete this line (currently line 5):

```rust
use bevy::window::WindowMode;
```

`WindowMode` is used nowhere else in this file after Step 4.

- [ ] **Step 3: Update the function signature**

Change (currently lines 19-25):

```rust
pub fn handle_navigation_actions(
    mut actions: MessageReader<'_, '_, ActionInput>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut windows: Query<'_, '_, &mut Window>,
) {
```

to:

```rust
pub fn handle_navigation_actions(
    mut actions: MessageReader<'_, '_, ActionInput>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut display: ResMut<'_, crate::settings::DisplaySettings>,
) {
```

- [ ] **Step 4: Replace the fullscreen block**

Change (currently lines 62-71):

```rust
    if fullscreen {
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(mode = ?window.mode, "toggle fullscreen");
        }
    }
```

to:

```rust
    if fullscreen {
        // Only the flag flips here. `crate::lifecycle::display::apply_display_mode`
        // is the single writer of `Window::mode` / `CursorOptions::visible`;
        // it is ordered `.after(handle_navigation_actions)` in `Update`, so
        // this toggle takes effect the same frame, and it re-derives the
        // target from DisplaySettings plus the live monitor set rather than
        // this handler guessing at a MonitorSelection directly.
        display.start_fullscreen = !display.start_fullscreen;
        tracing::info!(
            start_fullscreen = display.start_fullscreen,
            "toggle fullscreen"
        );
    }
```

- [ ] **Step 5: Update the module doc**

At the top of `crates/wc-core/src/lifecycle/nav.rs`, the module doc currently reads:

```rust
//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).
```

Change to:

```rust
//! Translates [`WaveConductorAction`] presses into [`AppState`] transitions and
//! window-level effects (fullscreen toggle).
//!
//! The fullscreen toggle only flips `DisplaySettings::start_fullscreen`
//! (`crate::settings::DisplaySettings`); it does not write `Window` directly.
//! `crate::lifecycle::display::apply_display_mode` is the sole writer of
//! `Window::mode` and `CursorOptions::visible`, ordered to run immediately
//! after this system each frame.
```

- [ ] **Step 6: Run the build and existing tests to verify nothing broke**

Run: `cargo check -p wc-core 2>&1 | head -30`

Expected: PASS, no errors, no unused-import or unused-variable warnings.

Run: `cargo nextest run -p wc-core`

Expected: PASS — this is a refactor of production code with no new automated coverage of its own; Task 1's tests already cover the logic `apply_display_mode` calls, and this step only confirms the crate still compiles and every existing test still passes.

- [ ] **Step 7: Manual smoke test — human required, `cargo rund`**

There are no GPU tests in CI and `cargo xtask capture` returns black frames for a backgrounded window (Part 1 of the program index), so this step cannot be automated. A human runs `cargo rund` and confirms:

1. The window opens **windowed** at roughly 1280×720 (debug default `start_fullscreen = false`), not fullscreen.
2. Pressing **F11** switches to fullscreen on the current monitor; the sketch fills the whole screen (this depends on Plan 02 already being merged — if the particle field still only fills the old windowed extent, Plan 02 is not actually in this branch, not a defect in this plan).
3. Pressing **F11** again returns to windowed at the original size.
4. Opening the settings panel (Shift+D or the cog), finding the new **Display** section, and toggling **Hide cursor** on: the OS mouse cursor disappears while it is over the window. Toggling it off brings the cursor back.
5. Typing a bogus value into the **Monitor** text field (e.g. `"Nonexistent Display"`) and pressing F11: the app still goes fullscreen (falls back to `Current`), and quitting and relaunching `cargo rund` still shows the bogus value in the field — it must not have been silently cleared.
6. If a second monitor is available: setting **Monitor** to that monitor's exact name and pressing F11 puts the fullscreen window on that monitor, not the primary one. This step is hardware-dependent and best-effort — skip if only one display is attached.

Multi-hour "TV sleeps and wakes" re-assertion (the actual field-reported bug this plan fixes) cannot be verified on a dev machine at all; it requires the deployment hardware and is covered by the 8-hour soak procedure in AGENTS.md, not by this step.

- [ ] **Step 8: Run the full gate and commit**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
git add crates/wc-core/src/lifecycle/nav.rs
git commit -F <message file>
```

Message:

```
refactor(lifecycle): F11 flips DisplaySettings.start_fullscreen, not Window

handle_navigation_actions no longer computes a WindowMode or touches
Window directly. crate::lifecycle::display::apply_display_mode (ordered
immediately after this system in Update) is now the single writer of
Window::mode and CursorOptions::visible, re-deriving the target from
DisplaySettings plus the live monitor set every frame. Keeps F11, a
settings-panel edit, and a monitor add/remove from being three code paths
that can disagree about what "fullscreen" currently means.
```

---

## Task 4: Upgrade `monitor` to Plan 03a's runtime-enumerated widget

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user/display.rs` (add `impl RuntimeEnumOptionsSource for AvailableMonitors`; change the `monitor` field's `#[setting(...)]` attribute; update its doc comment)
- Modify: `crates/wc-core/src/lifecycle/display.rs` (`DisplayPlugin::build`: register the options source)

**Interfaces:**
- Consumes, all from Plan 03a's shipped API (`docs/superpowers/plans/2026-07-09-alpha5-03a-runtime-enum-widget.md`, re-exported from `crate::settings`):
  - `pub trait RuntimeEnumOptionsSource: bevy::prelude::Resource { const OPTIONS_KEY: &'static str; fn options(&self) -> &[String]; }`
  - `pub trait RegisterRuntimeEnumOptionsExt { fn register_runtime_enum_options<R: RuntimeEnumOptionsSource>(&mut self) -> &mut Self; }` (impl'd for `App`)
  - `SettingKind::RuntimeEnum { options_key }` and the derive attribute `#[setting(ty = RuntimeEnum, options_key = "...")]` (03a's macro validates `options_key` is present at compile time)
  - plus `AvailableMonitors` (this plan's Task 1) and its populating system `sync_available_monitors` (Task 2).
- Produces: no change to `DisplaySettings`'s stored shape, persistence format, or resolution semantics — only the panel widget. `AvailableMonitors` gains a `RuntimeEnumOptionsSource` impl keyed `"monitors"`.

The `"monitors"` key appears in exactly two places and they must match: the `const OPTIONS_KEY` on the impl (Step 3) and the field's `options_key = "monitors"` attribute (Step 4). 03a's panel matches the field's key against each registered source's `OPTIONS_KEY` at render time — a typo in either yields an empty dropdown (03a's contract: an unresolved key is an empty option list, and the field falls back to the free-text escape hatch), so keep them literally identical. Step 7 adds a CI unit test that fails if they ever drift, so this is enforced, not just documented.

- [ ] **Step 1: Confirm Plan 03a has landed with the expected symbols**

Run: `rg -n "RuntimeEnum|register_runtime_enum_options|RuntimeEnumOptionsSource" crates/wc-core-macros/src/lib.rs crates/wc-core/src/settings/runtime_enum.rs crates/wc-core/src/settings/def.rs`

Expected: `crates/wc-core-macros/src/lib.rs` shows a `RuntimeEnum` `Kind` arm and an `options_key` attribute; `crates/wc-core/src/settings/runtime_enum.rs` defines `RuntimeEnumOptionsSource` and `RegisterRuntimeEnumOptionsExt`; `crates/wc-core/src/settings/def.rs` has a `SettingKind::RuntimeEnum { options_key }` variant. If any is absent, Plan 03a has not merged into this branch yet — stop; `DisplaySettings.monitor` keeps working as the plain `Text` field from Task 1 until 03a lands. (If the shipped symbol names differ from what this task cites, prefer the code over this plan and adjust the three references below — `OPTIONS_KEY`, `register_runtime_enum_options`, `options_key` — accordingly; nothing in Tasks 1-3 is affected, because none of that code goes through 03a's API.)

- [ ] **Step 2: Import the trait in `settings/panel_user/display.rs`**

In `crates/wc-core/src/settings/panel_user/display.rs`, the `use` block (from Task 1) currently reads:

```rust
use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;
```

Add the trait import:

```rust
use bevy::prelude::*;
use bevy::window::{MonitorSelection, WindowMode};
use serde::{Deserialize, Serialize};
use wc_core_macros::SketchSettings;

use crate::settings::RuntimeEnumOptionsSource;
```

- [ ] **Step 3: Implement `RuntimeEnumOptionsSource` for `AvailableMonitors`**

In the same file, immediately below the `AvailableMonitors` struct definition, add:

```rust
/// Feeds the live monitor-name list to Plan 03a's runtime-enum settings
/// widget. The `monitor` field of [`DisplaySettings`] declares
/// `options_key = "monitors"` (below), and 03a's panel resolves that key
/// against every registered [`RuntimeEnumOptionsSource`] at render time — so
/// this key and the field's must stay identical.
impl RuntimeEnumOptionsSource for AvailableMonitors {
    const OPTIONS_KEY: &'static str = "monitors";

    fn options(&self) -> &[String] {
        &self.0
    }
}
```

- [ ] **Step 4: Change the `monitor` field's attribute and doc comment**

In the same file, change the field (from Task 1):

```rust
    /// Monitor to target when `start_fullscreen` is true, persisted by
    /// **name** (not by winit's per-boot enumeration index, which is not
    /// stable across reboots or monitor topology changes). Empty string means
    /// "no preference" and resolves to [`MonitorSelection::Current`] via
    /// [`resolve_monitor_selection`].
    ///
    /// Renders as a plain text field until Task 4 upgrades it to Plan 03a's
    /// runtime-enumerated dropdown. The stored value and its resolution
    /// semantics are identical before and after that upgrade; only the widget
    /// changes.
    #[setting(
        default = String::new(),
        ty = Text,
        section = "Display",
        category = User,
        label = "Monitor"
    )]
    #[serde(default)]
    pub monitor: String,
```

to:

```rust
    /// Monitor to target when `start_fullscreen` is true, persisted by
    /// **name** (not by winit's per-boot enumeration index, which is not
    /// stable across reboots or monitor topology changes). Empty string means
    /// "no preference" and resolves to [`MonitorSelection::Current`] via
    /// [`resolve_monitor_selection`].
    ///
    /// Rendered by Plan 03a's runtime-enum widget as a dropdown populated from
    /// the [`AvailableMonitors`] source registered under `"monitors"`. A saved
    /// name that no longer resolves (an unplugged or sleeping monitor) is not
    /// dropped: 03a's widget shows it, marks it unavailable, and keeps it
    /// persisted, matching `resolve_monitor_selection`'s fall-back-without-
    /// rewrite behaviour. The stored value (a plain `String`) and its
    /// resolution semantics are unchanged from the `ty = Text` version; only
    /// the widget differs.
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "monitors",
        section = "Display",
        category = User,
        label = "Monitor"
    )]
    #[serde(default)]
    pub monitor: String,
```

- [ ] **Step 5: Register the options source in `DisplayPlugin::build`**

In `crates/wc-core/src/lifecycle/display.rs`, the `use` block (from Task 2) currently reads:

```rust
use crate::settings::{compute_display_mode, AvailableMonitors, DisplaySettings};
use crate::settings::RegisterSketchSettingsExt;
```

Add the registration extension trait:

```rust
use crate::settings::{compute_display_mode, AvailableMonitors, DisplaySettings};
use crate::settings::{RegisterRuntimeEnumOptionsExt, RegisterSketchSettingsExt};
```

Then in `DisplayPlugin::build`, add the registration next to `init_resource::<AvailableMonitors>()`. The existing block:

```rust
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<DisplaySettings>();
        app.init_resource::<AvailableMonitors>();
```

becomes:

```rust
    fn build(&self, app: &mut App) {
        app.register_sketch_settings::<DisplaySettings>();
        app.init_resource::<AvailableMonitors>();
        // Expose the live monitor list to Plan 03a's runtime-enum widget so
        // the `monitor` setting renders as a dropdown. `AvailableMonitors`
        // impls `RuntimeEnumOptionsSource` with `OPTIONS_KEY = "monitors"`,
        // matching the field's `options_key`.
        app.register_runtime_enum_options::<AvailableMonitors>();
```

- [ ] **Step 6: Run the existing tests to verify the stored-value contract is unchanged**

Run: `cargo test -p wc-core --lib settings::panel_user::display lifecycle::display`

Expected: PASS — the 12 `display` tests from Task 1 and the 1 `lifecycle::display` test from Task 2. None exercise the macro-generated `SettingDef`/widget path or the registration, only `DisplaySettings`'s plain fields, the pure functions, and `DisplayPlugin`'s resource set, so an attribute + impl + registration change cannot break them. If any fail, the field's Rust type or default changed, which this task must not do.

- [ ] **Step 7: Pin the field's `options_key` to the source's `OPTIONS_KEY` with a unit test**

The `"monitors"` key is written in two different files (Step 3's `impl` and Step 4's attribute), and 03a resolves an unknown key to an *empty option list* with a free-text fallback — so a drift between the two produces a silently empty dropdown at runtime, not a build error. The manual smoke test (Step 8) catches it on the day; nothing catches a later rename six months on. Pin them together with a compile-time-adjacent regression test.

Add to the `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/settings/panel_user/display.rs` (created in Task 1). First extend that block's imports — its header currently reads:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
```

Change the import line to bring the `SketchSettings` **trait** (for `settings_def()`) and `SettingKind` (for the destructure) into scope. `super::*` glob-imports the module's `use wc_core_macros::SketchSettings;`, but that is the *derive macro* — a different namespace from the trait, so the two names coexist without an E0252 conflict, and calling the trait method needs the trait explicitly:

```rust
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::settings::{SettingKind, SketchSettings};
```

Then append this test to the block (after the existing `available_monitors_defaults_empty` test):

```rust
    /// The derive macro's `options_key` (on the `monitor` field) and the
    /// options source's `OPTIONS_KEY` (on `AvailableMonitors`) are written in
    /// two different files; 03a resolves an unknown key to an empty option
    /// list, so a drift between them degrades the dropdown silently rather
    /// than failing the build. Pin them together.
    ///
    /// `unreachable!` (not `panic!`) on the two structural invariants: the
    /// derive macro always emits a def for every field, and `monitor` is
    /// declared `ty = RuntimeEnum`, so neither `else` arm is reachable unless
    /// the struct definition above changed out from under this test. Bare
    /// `panic!` is `warn`-then-denied (`Cargo.toml:206-211`); `clippy::unreachable`
    /// is a `restriction` lint and is not enabled, so `unreachable!` is clean —
    /// the same pattern `lifecycle/screensaver/run_condition.rs:66` already uses.
    #[test]
    fn monitor_field_options_key_matches_its_options_source() {
        let Some(def) = DisplaySettings::settings_def()
            .into_iter()
            .find(|d| d.field_name == "monitor")
        else {
            unreachable!("the derive macro always emits a def for `monitor`");
        };
        let SettingKind::RuntimeEnum { options_key } = def.kind else {
            unreachable!("`monitor` is declared `ty = RuntimeEnum`");
        };
        assert_eq!(options_key, AvailableMonitors::OPTIONS_KEY);
    }
```

`AvailableMonitors::OPTIONS_KEY` resolves through the `RuntimeEnumOptionsSource` trait imported at module level in Step 2 (visible here via `use super::*;`), so no additional import is needed for it.

Run: `cargo test -p wc-core --lib settings::panel_user::display`

Expected: PASS, now 13 tests (the 12 from Task 1 plus this one). If the new test fails to compile with "no variant `RuntimeEnum`", Step 4's attribute change or Plan 03a's `SettingKind` variant is missing; if it fails at the `assert_eq!`, the two `"monitors"` literals have drifted — fix whichever is wrong, do not weaken the assertion.

- [ ] **Step 8: Manual smoke test — human required, `cargo rund`**

There are no GPU tests in CI and `cargo xtask capture` returns black frames for a backgrounded window, so this cannot be automated. A human runs `cargo rund` and confirms in the settings panel's Display section:

1. The **Monitor** row now renders as a **dropdown** (not a free-text box), listing the currently attached monitor(s) by name. With one monitor attached, the dropdown shows that one name rather than being empty.
2. Selecting a monitor from the dropdown and pressing F11 puts the fullscreen window on that monitor (hardware-dependent; best-effort with only one display).
3. If Plan 03a's "unavailable" affordance is testable on this hardware: a persisted name for a now-absent monitor still appears in the row, marked unavailable, and is not silently cleared. (This is 03a's widget behaviour, re-confirmed here only because `DisplaySettings` is its first real consumer.)

- [ ] **Step 9: Run the full gate and commit**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
git add crates/wc-core/src/settings/panel_user/display.rs crates/wc-core/src/lifecycle/display.rs
git commit -F <message file>
```

Message:

```
feat(settings): render Monitor as Plan 03a's runtime-enumerated dropdown

AvailableMonitors now impls Plan 03a's RuntimeEnumOptionsSource (keyed
"monitors") and is registered via register_runtime_enum_options in
DisplayPlugin::build. The monitor field switches from ty = Text to
ty = RuntimeEnum, options_key = "monitors", so the settings panel renders
it as a dropdown of live monitor names instead of a hand-typed string.

No change to the stored value (still a plain String), its default, or its
resolution semantics -- resolve_monitor_selection and compute_display_mode
are untouched, and a saved-but-absent monitor name is kept, not rewritten.
```

---

## Self-Review

**Spec coverage.** Implements Workstream 4 of `docs/superpowers/specs/2026-07-08-windows-remediation-design.md` (fullscreen and display settings), scoped per the alpha.5 program index's Plan 03 entry. `boot_into_attract` does not appear anywhere in this plan — it is cut (index Part 1 / design doc §3 Non-goals), and the residual kiosk-boot gap it leaves is recorded in the index's Part 4 as unowned, not addressed here. The "Audio output" row the design doc's Workstream 4 originally listed alongside Display is **not** in this plan's Display section — the index's Part 3 correction assigns it to Plan 04's own `AudioSettings`, and this plan's Display section carries only Start fullscreen, Hide cursor, and Monitor.

**Correction from the original design assumption.** Both the design doc and the program index describe re-asserting fullscreen/monitor "on `MonitorAdded` / `MonitorRemoved`". Verified against vendored `bevy_window-0.19.0` and `bevy_winit-0.19.0` source: **no such message type exists in Bevy 0.19.** Monitors are plain `Monitor` ECS components spawned/despawned by `bevy_winit::system::create_monitors`, called once per event-loop iteration from `about_to_wait`, with no dedicated message. This plan substitutes an unconditional per-frame idempotent re-apply (`apply_display_mode`, Task 2) for the nonexistent event subscription — see that task's module doc for the full reasoning, including why it degrades gracefully around the `Startup`-vs-`create_monitors` race.

**Plan 03a dependency (verified, not assumed).** Plan 03a has shipped (`docs/superpowers/plans/2026-07-09-alpha5-03a-runtime-enum-widget.md`); Task 4 is written against its concrete contract — the `RuntimeEnumOptionsSource` trait (`const OPTIONS_KEY`, `fn options(&self) -> &[String]`), the `register_runtime_enum_options::<R>()` `App` extension, `SettingKind::RuntimeEnum { options_key }`, and the `#[setting(ty = RuntimeEnum, options_key = "...")]` derive attribute (a compile-time-validated string-literal key, *not* a resource type path). Cited in the header (`Depends on:`) and in Task 4's Interfaces block. Tasks 1-3 build the full, correct, working feature (settings, resolution logic, application systems, F11 rewire) using a plain `Text` widget for `monitor` and touch none of 03a's API; Task 4 adds the `impl RuntimeEnumOptionsSource for AvailableMonitors` (keyed `"monitors"`), the `register_runtime_enum_options::<AvailableMonitors>()` call in `DisplayPlugin::build`, and the field-attribute swap. It is isolated and can slip if 03a is not yet on the branch (Task 4, Step 1 checks) without blocking the rest.

**Type consistency.** `DisplaySettings.monitor: String` (empty = unset) throughout; `resolve_monitor_selection` and `compute_display_mode` both take `&str` / `impl IntoIterator<Item = (Entity, Option<&str>)>` and never mutate the saved name, which is what makes "never silently rewrite an unresolvable name" a structural property rather than a discipline. `AvailableMonitors(Vec<String>)` is produced by Task 1, populated by Task 2's `sync_available_monitors`, and exposed to 03a's widget in Task 4 via `impl RuntimeEnumOptionsSource` (`fn options(&self) -> &[String]` returns `&self.0`) — the same type at every step. The `"monitors"` key is written in exactly two places (the impl's `OPTIONS_KEY` and the field's `options_key`); Task 4, Step 7 **enforces** their equality with a CI unit test (`monitor_field_options_key_matches_its_options_source`) that reads the generated `SettingDef` and asserts `options_key == AvailableMonitors::OPTIONS_KEY`, rather than leaving the match to a code comment — because 03a degrades an unknown key to a silently empty dropdown, which no build error or manual pass reliably catches on a later rename.

**Ordering.** Task 1 → Task 2 → Task 3 are strictly ordered (each introduces the production caller the previous task's transient `dead_code` allow is waiting for). Task 4 must run last and may be deferred independently of the other three if Plan 03a has not landed yet.

**Placeholder scan.** No `TODO`, `TBD`, or `similar to Task N` appears in any code block. Every code block is complete and compilable, including Task 4's — its symbols (`RuntimeEnumOptionsSource`, `register_runtime_enum_options`, `SettingKind::RuntimeEnum`, `options_key = "monitors"`) are Plan 03a's shipped API, not guesses; Task 4, Step 1 is a sanity check that 03a is present on the branch, not a hedge against an invented API.

**Clippy self-check against the index's Part 1 traps.** No `.expect()`/`.unwrap()` outside a `#[allow(clippy::expect_used, reason = "...")]`-scoped test module (Task 1, Step 1). No `assert_eq!(x, true)` — every boolean assertion uses `assert!`/`assert!(!...)`. No `0..(N+1)` ranges anywhere in this plan's code. No `Box::leak`. No `unwrap()` on `Entity::from_raw_u32` outside the allowed test module. Task 4, Step 7's test uses `unreachable!` (not bare `panic!`) for its two structural invariants: `panic` is `warn`/denied in `Cargo.toml:206-211`, but `clippy::unreachable` is a `restriction` lint left disabled, so `unreachable!` passes `-D warnings` — verified against `lifecycle/screensaver/run_condition.rs:66`, which already uses it in a test.
