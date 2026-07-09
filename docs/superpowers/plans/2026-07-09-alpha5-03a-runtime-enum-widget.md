# Runtime-Enumerated Setting Widget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one new `SettingKind` whose dropdown options are supplied at runtime by a registered resource, so the settings panel can render a monitor picker (Plan 03) and an audio-device picker (Plan 04) as dropdowns instead of hand-typed strings, without either plan touching the other's code.

**Architecture:** `SettingKind::RuntimeEnum { options_key }` persists exactly like `SettingKind::Text` — the field stays a plain `String`, so persistence needs no change. What is new is a small generic registry (`RuntimeEnumOptionsSource` trait + `RuntimeEnumOptionsRegistry`, mirroring the existing `CustomDockSections` pattern) that lets an unrelated module (Plan 03's monitor enumerator, Plan 04's audio-device enumerator) register a `Resource` under a string key without the settings panel ever naming that resource's concrete type. The panel snapshots every registered source once per frame — before the reflected-field borrow that would make `world` unavailable — and the new widget resolves a field's `options_key` against that snapshot. A persisted value absent from the live list is never dropped: it is shown, marked unavailable, and stays directly editable via a free-text field alongside the dropdown.

**Tech Stack:** Rust, Bevy 0.19 (ECS `World`/`App`/`Resource`), `bevy_egui` 0.40, the `wc-core-macros` proc-macro crate (`#[derive(SketchSettings)]`).

## Global Constraints

Copied verbatim from `AGENTS.md` and the alpha.5 program index's Part 1 (`docs/superpowers/plans/2026-07-09-alpha5-program-index.md`). Every task's requirements implicitly include this section.

- **CI gates**, all of which must pass before a task is complete:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features --workspace -- -D warnings` — **use `--all-targets`, not `--lib`**, when scoping a task-local check; `--lib` skips the test target and hides lints in your own test code (this bit Plan 01 twice).
  - `cargo nextest run --workspace --all-features` (nextest does **not** run doctests)
  - `cargo test --doc --workspace`
  - `cargo doc --no-deps --workspace --document-private-items` — CI runs this with `RUSTDOCFLAGS="-D warnings"`. **Do not** reproduce it with `--all-features` (the real CI invocation has none); and a **public** item's rustdoc linking to a `pub(crate)` or private item trips `rustdoc::private_intra_doc_links`, which is denied. Use a plain code span (backticks, no `[...]`) for anything not fully `pub`.
  - `cargo deny check`
  - `cargo xtask check-secrets`
- **Clippy is `-D warnings` over `pedantic`, including inside `#[cfg(test)]`.** `.expect()` / `.unwrap()` in a `#[cfg(test)] mod tests` block is denied unless the file already carries a scoped `#[allow(clippy::expect_used, reason = "...")]` (several files touched by this plan already do — reuse it, don't fight it). `assert_eq!(x.is_some(), true)` → use `assert!(x.is_some())`. `0..(N + 1)` → use `0..=N`.
- **No `unwrap()` or `expect()` in non-test code** unless the panic is a documented invariant violation. (`wc-core-macros` carries a crate-wide `#![allow(clippy::expect_used, reason = "expect with a clear message is appropriate inside proc-macro code paths")]` at `lib.rs:64-67` — proc-macro code in that crate is exempt by design; nothing else is.)
- **No `as` casts on numeric types** where `From`/`TryFrom`/`u32::try_from` would work. (Not expected to come up in this plan — no numeric fields are added.)
- `///` rustdoc on every public item (struct, enum, trait, fn, module). Module-level `//!` on every module root.
- **Never allocate in a hot path**: per-frame Bevy systems, egui paint-callback `update`/`render` hooks, the audio callback, and continuously-running worker/background threads. This plan's widget renders inside the settings panel's per-frame egui pass, which **is** a per-frame Bevy system — but it only runs while the panel is visible (gated off by default), not for the life of the session, and the existing `render_file_path` widget in the same file already clones a `String` every frame it renders (`panel_user/widgets.rs:325-330`). Task 3 documents the one small allocation this plan adds (a fresh `Arc<[String]>` snapshot per registered source, once per panel-visible frame) against that precedent rather than pretending it doesn't exist — see Task 3's design note.
- **There are no GPU tests in CI.** `crates/wc-core/tests/ui_blur.rs` is entirely `#[ignore]`d because `DefaultPlugins` needs the macOS main thread. Nothing in this plan touches rendering or needs a GPU — every test here is a plain ECS `World`/`App` or pure-function unit test — but if a future plan is tempted to add a capture-based check for this widget, it won't run in CI.
- **Commit messages: `-F <file>`, never `-m`.** Backticks inside a `-m` string are command-substituted by zsh and silently eat words. Write the message to a file (e.g. `/tmp/wc-plan03a-taskN.txt`) and `git commit -F <file>`.
- **Never `git add -A`.** Stage named paths only, then `git show --stat HEAD` to confirm.
- **Test-only helpers need `#[cfg(test)]`, and a type with no non-test caller is dead code.** An accessor used only from `mod tests` trips `dead_code` on the lib target under `-D warnings`. Task 2 introduces `RuntimeEnumSelection`/`classify_runtime_enum_selection` before their production caller exists (Task 3); it carries a transient `#[allow(dead_code)]` with an explicit note of which task removes it, and Task 3 removes it and re-verifies clippy stays clean without it.
- **Adding a `SettingKind` variant is a non-exhaustive-match break.** `rg -n "SettingKind::" crates/` was run for this plan; the complete list of production, non-test match sites that require a new arm is enumerated below, in the final report and in Task 3/4.

---

### Task 1: Runtime-enum options registry and registration API

**Files:**
- Create: `crates/wc-core/src/settings/runtime_enum.rs`
- Modify: `crates/wc-core/src/settings/mod.rs:26-48` (module declaration + re-exports)
- Modify: `crates/wc-core/src/settings/mod.rs:59-74` (`SettingsPlugin::build`, register the new resource)

**Interfaces:**
- Consumes: nothing from other tasks. Fully self-contained; does not touch `SettingKind` or any existing match site.
- Produces:
  - `pub trait RuntimeEnumOptionsSource: bevy::prelude::Resource { const OPTIONS_KEY: &'static str; fn options(&self) -> &[String]; }`
  - `pub trait RegisterRuntimeEnumOptionsExt { fn register_runtime_enum_options<R: RuntimeEnumOptionsSource>(&mut self) -> &mut Self; }` + `impl RegisterRuntimeEnumOptionsExt for bevy::prelude::App`
  - `#[derive(Resource, Default)] pub struct RuntimeEnumOptionsRegistry { .. }`
  - `pub(crate) struct RuntimeEnumOptionsSnapshotEntry { pub(crate) options_key: &'static str, pub(crate) options: std::sync::Arc<[String]> }`
  - `pub(crate) type RuntimeEnumOptionsSnapshot = smallvec::SmallVec<[RuntimeEnumOptionsSnapshotEntry; 4]>;`
  - `pub(crate) fn snapshot(world: &bevy::prelude::World) -> RuntimeEnumOptionsSnapshot`
  - `pub(crate) fn options_for<'a>(snapshot: &'a [RuntimeEnumOptionsSnapshotEntry], options_key: &str) -> &'a [String]`

This is the "how does the panel locate the right options `Resource`" design, so the reasoning is worth stating before the code: `SettingKind` is a `'static` compile-time table (the whole point of `SettingDef` per `crates/wc-core/src/settings/def.rs:1-5` is that a panel renders it without reflection-walking the struct every frame), so a `SettingKind::RuntimeEnum` variant cannot embed a concrete resource type or a live value — only a `&'static str` key. Something has to map that key to a live `Vec<String>` at render time without the panel crate knowing about `AvailableAudioDevices` or `AvailableMonitors` (types that don't exist yet and belong to Plans 04 and 03 respectively). `crates/wc-core/src/settings/custom_section.rs` already solves an identically-shaped problem — "let an unrelated module contribute per-frame UI behaviour without the panel naming its type" — via a `Vec` of type-erased function pointers behind an `App` extension trait (`CustomDockSections` / `register_dock_section`). This task copies that shape: `register_runtime_enum_options::<R>()` records `(R::OPTIONS_KEY, a fn pointer that reads R out of a World)` in a registry `Resource`; nothing about `R` itself is stored generically. The panel then calls `snapshot(world)` once per frame (Task 3 wires the call site) to get every registered source's *current* options as owned, ref-counted data it can hold across the reflected-field borrow that follows.

- [ ] **Step 1: Write the failing test**

Create `crates/wc-core/src/settings/runtime_enum.rs` with just the module doc, imports, and test module (referencing types that don't exist yet, so it fails to compile):

```rust
//! Registry of runtime-enumerated option sources for
//! `crate::settings::SettingKind::RuntimeEnum` fields.
//!
//! ## Why this exists
//!
//! `SettingKind::Enum`'s variant list is fixed at compile time — the derive
//! macro reads it off the field's Rust enum type via reflection. A monitor
//! list or an audio-device list is not known until the OS enumerates
//! hardware at runtime, so it cannot be a Rust enum at all: the persisted
//! value is a plain `String` (a device or monitor name), and what varies is
//! only *which strings are currently selectable*.
//!
//! `RuntimeEnumOptionsSource` is a small trait a module implements on a
//! `Resource` it already owns (e.g. an `AvailableAudioDevices(Vec<String>)`
//! populated by a `cpal` enumeration system).
//! `RegisterRuntimeEnumOptionsExt::register_runtime_enum_options` records
//! that resource's type against an `options_key` string. The settings panel
//! never names the concrete
//! resource type: `snapshot` walks every registered entry through its
//! stored function pointer and returns a small keyed list, which the panel
//! matches against each field's `SettingKind::RuntimeEnum { options_key }` at
//! render time (see `panel_user::widgets::render_runtime_enum`, added in
//! Task 3 of this plan).
//!
//! This indirection is what lets two unrelated consumers (a monitor picker
//! and an audio-device picker) each register their own resource type without
//! either one editing the shared panel code — the same generic-registry
//! shape `super::custom_section` already uses for sketch-contributed dock
//! sections, and `super::registry` uses for `SketchSettings` types
//! themselves.
//!
//! ## What this does *not* do
//!
//! `register_runtime_enum_options::<R>()` only records the `(options_key,
//! snapshot_fn)` pair. It does not insert, update, or own the `R` resource —
//! that stays the registering module's responsibility (a startup system that
//! enumerates hardware plus an `insert_resource`/`init_resource` call, or a
//! periodic system that refreshes it). `snapshot` simply reads whatever is
//! present at call time and omits a key silently when its resource is absent
//! (e.g. enumeration hasn't completed yet).
//!
//! ## Persistence is unaffected
//!
//! The field this feeds is still a plain `String` — `SettingKind::RuntimeEnum`
//! persists exactly like `SettingKind::Text` (a TOML string). Only the
//! *widget* differs; see `panel_user::widgets::render_runtime_enum`.

use std::sync::Arc;

use bevy::prelude::*;

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct FakeAudioDevices(Vec<String>);

    impl RuntimeEnumOptionsSource for FakeAudioDevices {
        const OPTIONS_KEY: &'static str = "audio_output_devices";
        fn options(&self) -> &[String] {
            &self.0
        }
    }

    #[derive(Resource, Default)]
    struct FakeMonitors(Vec<String>);

    impl RuntimeEnumOptionsSource for FakeMonitors {
        const OPTIONS_KEY: &'static str = "display_monitors";
        fn options(&self) -> &[String] {
            &self.0
        }
    }

    #[test]
    fn snapshot_is_empty_before_anything_is_registered() {
        let world = World::new();
        assert!(snapshot(&world).is_empty());
    }

    #[test]
    fn snapshot_reads_every_registered_sources_current_options() {
        let mut app = App::new();
        app.register_runtime_enum_options::<FakeAudioDevices>();
        app.register_runtime_enum_options::<FakeMonitors>();
        app.insert_resource(FakeAudioDevices(vec![
            "HDMI TV".to_owned(),
            "Speakers".to_owned(),
        ]));
        app.insert_resource(FakeMonitors(vec!["DP-1".to_owned()]));

        let snap = snapshot(app.world());
        assert_eq!(
            options_for(&snap, "audio_output_devices").to_vec(),
            vec!["HDMI TV".to_owned(), "Speakers".to_owned()]
        );
        assert_eq!(
            options_for(&snap, "display_monitors").to_vec(),
            vec!["DP-1".to_owned()]
        );
    }

    #[test]
    fn a_registered_source_with_no_resource_inserted_yields_no_entry() {
        // Registration records the key; the resource itself may not exist
        // yet (e.g. audio enumeration hasn't run at startup). snapshot()
        // must skip that key, never panic or fabricate an entry.
        let mut app = App::new();
        app.register_runtime_enum_options::<FakeAudioDevices>();
        let snap = snapshot(app.world());
        assert!(options_for(&snap, "audio_output_devices").is_empty());
    }

    #[test]
    fn options_for_an_unregistered_key_is_an_empty_slice() {
        let snap = RuntimeEnumOptionsSnapshot::new();
        assert!(options_for(&snap, "nothing_registered").is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

First register the module. In `crates/wc-core/src/settings/mod.rs`, change the module list (currently lines 26-38):

```rust
pub mod autosave;
pub mod commands;
pub mod custom_section;
pub mod def;
pub mod event;
pub mod hand_tracking;
pub mod input_capture;
pub mod panel_dev;
pub mod persistence;
pub mod registry;
pub mod runtime_enum;
pub mod trait_def;

mod panel_user;
```

(Only the new `pub mod runtime_enum;` line is added, alphabetically between `registry` and `trait_def`; nothing else on this list changes.)

Run: `cargo test -p wc-core --lib settings::runtime_enum 2>&1 | head -30`

Expected: FAIL to compile — `cannot find type RuntimeEnumOptionsSource`, `RuntimeEnumOptionsRegistry`, `RuntimeEnumOptionsSnapshot`, `snapshot`, `options_for`, and the `register_runtime_enum_options` method, all "not found in this scope".

- [ ] **Step 3: Write the implementation**

Append to `crates/wc-core/src/settings/runtime_enum.rs`, between the `use bevy::prelude::*;` line and the `#[cfg(test)]` module:

```rust
/// A Bevy `Resource` that supplies the live option list for one or more
/// `crate::settings::SettingKind::RuntimeEnum` fields.
///
/// Implement this on a resource your module already owns (e.g. a list of
/// enumerated audio output devices or connected monitors), then register it
/// with `RegisterRuntimeEnumOptionsExt::register_runtime_enum_options`. The
/// settings panel never sees `Self` directly — it calls `Self::options`
/// through a type-erased function pointer captured at registration time (see
/// `snapshot`).
pub trait RuntimeEnumOptionsSource: Resource {
    /// Matched against a field's `SettingKind::RuntimeEnum { options_key }`.
    /// Two runtime-enum fields sharing a key would share one dropdown's
    /// option list, so pick something field-specific
    /// (`"audio_output_devices"`, `"display_monitors"`).
    const OPTIONS_KEY: &'static str;

    /// The current option list, in the order it should appear in the
    /// dropdown. Called at most once per settings-panel-visible frame per
    /// registered source (see `snapshot`), so this should be a cheap field
    /// read — not a fresh OS enumeration call.
    fn options(&self) -> &[String];
}

/// One registered source's option list, captured by `snapshot` for the
/// current frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeEnumOptionsSnapshotEntry {
    /// The source's `RuntimeEnumOptionsSource::OPTIONS_KEY`.
    pub(crate) options_key: &'static str,
    /// `RuntimeEnumOptionsSource::options` at snapshot time, ref-counted so
    /// cloning the snapshot list is a refcount bump, not a `Vec` copy.
    pub(crate) options: Arc<[String]>,
}

/// Inline stack snapshot of every registered source's current options.
/// Sized for the expected case of a couple of runtime-enumerated fields per
/// app (today: a monitor list and an audio-device list); spills to the heap
/// above that, same idiom as `panel_user::dock::KeySnapshot`.
pub(crate) type RuntimeEnumOptionsSnapshot = smallvec::SmallVec<[RuntimeEnumOptionsSnapshotEntry; 4]>;

/// Type-erased entry stored in `RuntimeEnumOptionsRegistry`.
#[derive(Clone)]
struct RuntimeEnumOptionsRegistryEntry {
    options_key: &'static str,
    /// Baked per `R` at registration time by
    /// `RegisterRuntimeEnumOptionsExt::register_runtime_enum_options`.
    /// Returns `None` when `R` is not currently present as a resource (not
    /// yet inserted, or removed) — `snapshot` then omits the key entirely
    /// rather than panicking or fabricating an empty entry.
    snapshot_fn: fn(&World) -> Option<Arc<[String]>>,
}

/// Registry of every `RuntimeEnumOptionsSource` type registered via
/// `RegisterRuntimeEnumOptionsExt::register_runtime_enum_options`.
///
/// Read only by `snapshot`. Mirrors `super::custom_section::CustomDockSections`'s
/// shape: a `Vec` of type-erased function pointers, populated by an `App`
/// extension trait, so unrelated modules can each contribute an entry
/// without editing this file.
#[derive(Resource, Default)]
pub struct RuntimeEnumOptionsRegistry {
    entries: Vec<RuntimeEnumOptionsRegistryEntry>,
}

/// Extension trait adding an `App::register_runtime_enum_options` method for
/// a module to contribute a runtime-enumerated option source.
pub trait RegisterRuntimeEnumOptionsExt {
    /// Register `R` as the options source for its
    /// `RuntimeEnumOptionsSource::OPTIONS_KEY`. Does not insert or manage
    /// `R` itself — insert it (and keep it updated) separately; see the
    /// module docs.
    fn register_runtime_enum_options<R: RuntimeEnumOptionsSource>(&mut self) -> &mut Self;
}

impl RegisterRuntimeEnumOptionsExt for App {
    fn register_runtime_enum_options<R: RuntimeEnumOptionsSource>(&mut self) -> &mut Self {
        self.world_mut()
            .get_resource_or_insert_with(RuntimeEnumOptionsRegistry::default)
            .entries
            .push(RuntimeEnumOptionsRegistryEntry {
                options_key: R::OPTIONS_KEY,
                snapshot_fn: snapshot_one::<R>,
            });
        self
    }
}

/// The snapshot closure baked per `R` at registration time. `None` when `R`
/// is not currently present as a resource.
fn snapshot_one<R: RuntimeEnumOptionsSource>(world: &World) -> Option<Arc<[String]>> {
    world.get_resource::<R>().map(|r| Arc::from(r.options()))
}

/// Snapshot every registered source's current options.
///
/// Called from `panel_user::fields::render_section_by_key` (wired in Task 3
/// of this plan), before the reflected field borrow it needs `world` for
/// makes `world` unavailable to the widgets that consume this snapshot — the
/// same ordering constraint the panel's own `SettingDef` list is read under.
///
/// `world.get_resource::<RuntimeEnumOptionsRegistry>()` and each entry's
/// `snapshot_fn(world)` are both shared (`&World`) borrows, so — unlike
/// `super::registry::emit_restart_events`, which snapshots its fn-pointer
/// list *before* re-entering `world` because its per-type functions need
/// `&mut World` — this reads the registry and calls every entry's
/// `snapshot_fn` in one pass without a two-phase snapshot.
pub(crate) fn snapshot(world: &World) -> RuntimeEnumOptionsSnapshot {
    let Some(registry) = world.get_resource::<RuntimeEnumOptionsRegistry>() else {
        return RuntimeEnumOptionsSnapshot::new();
    };
    registry
        .entries
        .iter()
        .filter_map(|entry| {
            (entry.snapshot_fn)(world).map(|options| RuntimeEnumOptionsSnapshotEntry {
                options_key: entry.options_key,
                options,
            })
        })
        .collect()
}

/// Look up the live option list for `options_key` inside a snapshot returned
/// by `snapshot`. Returns an empty slice when no registered source
/// currently reports that key (not yet enumerated, or no source registered
/// at all) — callers must not treat that as an error; the persisted value
/// still renders regardless (see `panel_user::widgets::render_runtime_enum`).
pub(crate) fn options_for<'a>(
    snapshot: &'a [RuntimeEnumOptionsSnapshotEntry],
    options_key: &str,
) -> &'a [String] {
    snapshot
        .iter()
        .find(|entry| entry.options_key == options_key)
        .map_or(&[], |entry| entry.options.as_ref())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib settings::runtime_enum`

Expected: PASS, 4 tests.

- [ ] **Step 5: Wire the registry resource into `SettingsPlugin`, and the re-exports**

In `crates/wc-core/src/settings/mod.rs`, add the re-export to the `pub use` block (currently lines 40-48), alphabetically between `registry::{...}` and `trait_def::SketchSettings`:

```rust
pub use commands::set_setting;
pub use custom_section::{CustomDockSections, DockSectionFn, RegisterDockSectionExt};
pub use def::{enum_variant_names, NumberRange, SettingDef, SettingKind, SettingsCategory};
pub use event::SketchRestart;
pub use hand_tracking::{HandProviderChoice, HandTrackingSettings};
pub use input_capture::{EguiKeyboardCaptured, EguiPointerCaptured};
pub use panel_dev::DevPanelVisible;
pub use registry::{RegisterSketchSettingsExt, SettingsRegistry};
pub use runtime_enum::{RegisterRuntimeEnumOptionsExt, RuntimeEnumOptionsRegistry, RuntimeEnumOptionsSource};
pub use trait_def::SketchSettings;
```

Then in `SettingsPlugin::build` (currently lines 59-74), add the resource init next to `CustomDockSections`:

```rust
impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsRegistry>()
            .init_resource::<DevPanelVisible>()
            .init_resource::<custom_section::CustomDockSections>()
            .init_resource::<runtime_enum::RuntimeEnumOptionsRegistry>()
            .init_resource::<autosave::AutosaveState>()
            .init_resource::<EguiPointerCaptured>()
            .init_resource::<EguiKeyboardCaptured>()
```

(The rest of `build` — the `SettingsPanelVisible` init, `add_message`, `add_systems`, and the trailing `panel_user::add_systems(app); panel_dev::add_systems(app);` calls — is unchanged.)

- [ ] **Step 6: Run the scoped gate and commit**

The workspace-wide clippy gate is deliberately not run here; the controller runs it between tasks.

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib settings::runtime_enum
cargo test -p wc-core --lib settings::mod
git add crates/wc-core/src/settings/runtime_enum.rs crates/wc-core/src/settings/mod.rs
```

Write the commit message to a file (e.g. `/tmp/wc-plan03a-task1.txt`):

```
feat(settings): add runtime-enum options registry

SettingKind::Enum's variant list is fixed at compile time from a Rust
enum's reflection info. A monitor list or an audio-device list is only
known once the OS enumerates hardware, so it can't be a Rust enum --
the field stays a plain String. RuntimeEnumOptionsSource lets a module
register a Resource it owns under a string key without the settings
panel ever naming that resource's concrete type, mirroring the
existing CustomDockSections registry shape. This is a prerequisite for
Plan 03's monitor picker and Plan 04's audio-device picker; neither
SettingKind nor any existing widget changes in this commit.
```

```bash
git commit -F /tmp/wc-plan03a-task1.txt
git show --stat HEAD
```

---

### Task 2: Pure selection-classification helper (TDD, extracted before its caller exists)

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user/widgets.rs` (new private items, plus the `#[cfg(test)] mod tests` block at the file footer, currently starting at line 389)

**Interfaces:**
- Consumes: nothing from Task 1 (this is pure `&str`/`&[String]` logic, no `World`, no egui).
- Produces:
  - `enum RuntimeEnumSelection { Empty, Known, Unavailable }` (private to `widgets.rs`, mirrors the file's existing `render_enum`'s privacy — no `pub` qualifier)
  - `fn classify_runtime_enum_selection(current: &str, options: &[String]) -> RuntimeEnumSelection`

This is the locked decision that most needs its own test: *"When the persisted name is absent from the live options list: show it, mark it unavailable, KEEP it persisted. Never silently rewrite the operator's choice."* Following the pattern Plan 01 used for `composite_uniforms` (extract the pure decision logic before wiring it into egui, so the contract is tested without a GPU or a UI context — see `docs/superpowers/plans/2026-07-08-alpha5-01-gpu-memory-leak.md` Task 2), this task extracts the classification as a standalone pure function first. It has no caller yet — `render_runtime_enum` doesn't exist until Task 3 — so it carries a transient `#[allow(dead_code)]`, exactly the pattern Plan 01's Task 1 (`SlotBook`) used and Task 4 removed.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/settings/panel_user/widgets.rs` (after the existing `every_listed_variant_is_writable` test, inside the same `mod tests { ... }`):

```rust
    #[test]
    fn classify_runtime_enum_selection_cases() {
        let opts = vec!["Speakers".to_owned(), "HDMI TV".to_owned()];
        assert_eq!(
            classify_runtime_enum_selection("", &opts),
            RuntimeEnumSelection::Empty
        );
        assert_eq!(
            classify_runtime_enum_selection("HDMI TV", &opts),
            RuntimeEnumSelection::Known
        );
        assert_eq!(
            classify_runtime_enum_selection("Living Room TV", &opts),
            RuntimeEnumSelection::Unavailable,
            "a persisted name absent from the live list must classify as \
             Unavailable, never silently dropped"
        );
    }

    #[test]
    fn classify_runtime_enum_selection_with_empty_live_list_is_unavailable_not_empty() {
        // No source registered yet, or enumeration hasn't run: the live
        // list is empty, but a persisted (non-empty) value must still
        // classify as Unavailable, not Empty -- Empty means "nothing
        // persisted," a different UI state (no "(unavailable)" marker, no
        // reset-cell affordance driven off it).
        assert_eq!(
            classify_runtime_enum_selection("Living Room TV", &[]),
            RuntimeEnumSelection::Unavailable
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib settings::panel_user::widgets 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function classify_runtime_enum_selection` and `cannot find type RuntimeEnumSelection`.

- [ ] **Step 3: Write the implementation**

Add above the existing `#[cfg(test)] mod tests` block in `crates/wc-core/src/settings/panel_user/widgets.rs` (i.e. immediately after `render_vec3`, before the test module):

```rust
/// Whether a runtime-enumerated field's persisted value is currently present
/// in its live option list. Pure and UI-free so the "never silently rewrite
/// an unresolved name" contract (an HDMI TV that is merely asleep must not
/// lose its saved binding — see `AGENTS.md`) is unit-tested directly, without
/// an egui context or a GPU.
//
// Transient. Has no non-test caller until Task 3 of this plan wires
// `render_runtime_enum` in, so the lib target (compiled without `cfg(test)`)
// sees both this and `classify_runtime_enum_selection` as dead code under
// `-D warnings`. Task 3 removes this attribute and the one below, and
// re-verifies clippy stays clean without them.
#[allow(
    dead_code,
    reason = "no non-test caller until alpha.5 Plan 03a Task 3 wires render_runtime_enum in"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeEnumSelection {
    /// No value persisted yet (`current` is empty).
    Empty,
    /// `current` matches one of the live options.
    Known,
    /// `current` is non-empty but does not appear in the live option list.
    /// Never treated as "reset me" — the caller must keep showing it and
    /// keep it selectable/editable.
    Unavailable,
}

/// Classify `current` against `options`. See `RuntimeEnumSelection`.
#[allow(
    dead_code,
    reason = "no non-test caller until alpha.5 Plan 03a Task 3 wires render_runtime_enum in"
)]
fn classify_runtime_enum_selection(current: &str, options: &[String]) -> RuntimeEnumSelection {
    if current.is_empty() {
        RuntimeEnumSelection::Empty
    } else if options.iter().any(|o| o == current) {
        RuntimeEnumSelection::Known
    } else {
        RuntimeEnumSelection::Unavailable
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib settings::panel_user::widgets`

Expected: PASS, including the two new tests alongside the file's existing tests.

- [ ] **Step 5: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core --lib settings::panel_user::widgets
git add crates/wc-core/src/settings/panel_user/widgets.rs
```

Message file (`/tmp/wc-plan03a-task2.txt`):

```
feat(settings/panel): extract classify_runtime_enum_selection, pure and tested

Whether a runtime-enum field's persisted value is currently live is a
pure decision (Empty / Known / Unavailable) with no egui or GPU
dependency, so it is extracted and unit-tested before the widget that
will use it exists -- same TDD move Plan 01 used for
composite_uniforms. Unavailable is the case that matters: a persisted
name absent from the live list (a sleeping HDMI TV, a device that
hasn't enumerated yet) must never be silently treated as unset.

Carries a transient #[allow(dead_code)] until alpha.5 Plan 03a Task 3
wires it into render_runtime_enum.
```

```bash
git commit -F /tmp/wc-plan03a-task2.txt
git show --stat HEAD
```

---

### Task 3: `SettingKind::RuntimeEnum`, the widget, and panel plumbing

**Files:**
- Modify: `crates/wc-core/src/settings/def.rs:54-61` (new `SettingKind` variant)
- Modify: `crates/wc-core/src/settings/commands.rs:63-79` (console `set` write-back — exhaustive match site #1)
- Modify: `crates/wc-core/src/settings/panel_user/widgets.rs:42-87` (exhaustive match site #2, plus the new `render_runtime_enum` + `options_for` call, plus removing Task 2's transient `#[allow(dead_code)]`)
- Modify: `crates/wc-core/src/settings/panel_user/fields.rs:33-105,138-149,195-219` (snapshot the options registry, thread it to the widget)
- Modify: `crates/wc-core/src/settings/panel_user/mod.rs:40-42` (module-doc widget list, cosmetic)

**Interfaces:**
- Consumes: `RuntimeEnumOptionsSnapshotEntry`, `RuntimeEnumOptionsSnapshot`, `snapshot()`, `options_for()` (Task 1); `RuntimeEnumSelection`, `classify_runtime_enum_selection()` (Task 2).
- Produces:
  - `SettingKind::RuntimeEnum { options_key: &'static str }`
  - `fn render_runtime_enum(field: &mut dyn PartialReflect, storage_key: &'static str, field_name: &'static str, options_key: &'static str, runtime_enum_options: &[RuntimeEnumOptionsSnapshotEntry], ui: &mut egui::Ui)`
  - `render_widget_value` gains a `runtime_enum_options: &[RuntimeEnumOptionsSnapshotEntry]` parameter.
  - `render_section_by_key` and `render_user_fields_via_reflect` (both in `fields.rs`) gain/propagate the same snapshot.

This is the task that makes `SettingKind` non-exhaustive against the existing matches, so — following the precedent in Plan 01's Task 3 ("add a field, fix both breaking call sites in the same task") — every match site it breaks is fixed inside this one task, not deferred.

**Design note carried from the Global Constraints section.** `render_section_by_key` (in `fields.rs`) will call `crate::settings::runtime_enum::snapshot(world)` once per frame, while the panel is visible, before it takes the reflected-field borrow. `snapshot()` allocates a fresh `Arc<[String]>` per registered source on that call (see Task 1's `snapshot_one`). This is a real per-frame allocation while the panel is open — but the panel is gated off by default (`SettingsPanelVisible` defaults `false`), it is UI-cadence code rather than the render-frame/audio-callback/worker-thread hot paths `AGENTS.md` is aimed at, and it is no worse than the file's existing `render_file_path`, which already clones/allocates a `String` every frame it renders a `FilePath` field (`panel_user/widgets.rs:325-330`, `.clone()` / `.to_string_lossy().into_owned()`). Not fixed here; if profiling ever flags the settings panel, the fix is to cache each source's `Arc<[String]>` on the `RuntimeEnumOptionsRegistryEntry` and only refresh it when the resource's own change-detection fires, rather than reallocating unconditionally every frame.

- [ ] **Step 1: Add the `SettingKind::RuntimeEnum` variant**

In `crates/wc-core/src/settings/def.rs`, insert a new variant into the `SettingKind` enum immediately after the existing `Enum { .. }` variant (currently lines 54-61, i.e. right before the enum's closing `}`):

```rust
    /// A `String`-valued setting whose selectable options are supplied at
    /// **runtime**, in contrast to `SettingKind::Enum` (whose `variants`
    /// list is fixed at compile time from a Rust enum's reflection info).
    /// Used for pickers whose candidates are only known once the OS
    /// enumerates hardware — an audio-output device list, a monitor list.
    ///
    /// Persists exactly like `SettingKind::Text` (a plain TOML string), so
    /// this kind required no persistence change; only the widget differs.
    /// Rendered as an `egui::ComboBox` sourced from whichever registered
    /// `crate::settings::RuntimeEnumOptionsSource` resource's
    /// `OPTIONS_KEY` matches `options_key`, plus a free-text field so a
    /// persisted name the live source doesn't currently report — a sleeping
    /// TV, a device unplugged mid-session — stays visible and directly
    /// editable rather than being silently reset. See
    /// `crate::settings::runtime_enum` for the resource-registration side.
    RuntimeEnum {
        /// Matched against
        /// `crate::settings::RuntimeEnumOptionsSource::OPTIONS_KEY` to find
        /// the resource supplying this field's live option list at render
        /// time. Distinct `options_key`s let two unrelated runtime-enum
        /// fields (e.g. a monitor picker and an audio-device picker) coexist
        /// without collision.
        options_key: &'static str,
    },
```

- [ ] **Step 2: Run the build to verify it breaks the two exhaustive match sites**

Run: `cargo check -p wc-core 2>&1 | head -40`

Expected: FAIL. Two non-exhaustive-match errors, both "pattern `SettingKind::RuntimeEnum { .. }` not covered":
- `crates/wc-core/src/settings/commands.rs` (the `match &def.kind` in `set_setting`)
- `crates/wc-core/src/settings/panel_user/widgets.rs` (the `match &def.kind` in `render_widget_value`)

These are the only two non-test, non-macro production sites that exhaustively match `SettingKind` in the whole tree (verified via `rg -n "SettingKind::" crates/` — see this plan's final report for the full list of every match/construction site, exhaustive or not).

- [ ] **Step 3: Fix `commands.rs`**

In `crates/wc-core/src/settings/commands.rs`, the `match &def.kind` in `set_setting` (currently lines 63-79) already folds `Text`, `FilePath`, and `TemplateLibrary` into one free-text write-back arm because all three are plain `String` fields. `RuntimeEnum` is the same shape, so it joins that arm:

```rust
    match &def.kind {
        SettingKind::Number(range) => apply_number(field_ref, range, value)?,
        SettingKind::Boolean => apply_bool(field_ref, value)?,
        SettingKind::Enum { variants } => apply_enum(field_ref, variants, value)?,
        // A file path / template path / runtime-enum value is stored as a
        // plain String, so a string value works. The console does not
        // validate against the live options list -- that is the same
        // free-text escape hatch the panel widget offers, just via `set`
        // instead of the TextEdit next to the ComboBox.
        SettingKind::Text
        | SettingKind::FilePath { .. }
        | SettingKind::TemplateLibrary { .. }
        | SettingKind::RuntimeEnum { .. } => {
            apply_text(field_ref, value)?;
        }
        SettingKind::Color => {
            return Err("color settings can't be set from the console; use the panel".to_owned());
        }
        SettingKind::TextList => {
            return Err("list settings can't be set from the console; use the panel".to_owned());
        }
    }
```

- [ ] **Step 4: Write the failing widget test**

Add to the `#[cfg(test)] mod tests` block at the footer of `crates/wc-core/src/settings/panel_user/widgets.rs`, after the tests added in Task 2. `value` is declared outside the `run_ui` closure and borrowed into it (matching how `reset_cell_unmodified_branch_is_grid_safe`, already in this file, threads a field through a real `egui::Grid`), so its post-render state is directly observable afterward:

```rust
    #[test]
    fn render_runtime_enum_keeps_persisted_value_when_absent_from_live_list() {
        let ctx = egui::Context::default();
        let mut value = String::from("Living Room TV");
        let snapshot = [crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry {
            options_key: "audio_output_devices",
            options: std::sync::Arc::from(["Speakers".to_owned()]),
        }];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            render_runtime_enum(
                field,
                "audio",
                "output_device",
                "audio_output_devices",
                &snapshot,
                ui,
            );
        });
        assert_eq!(
            value, "Living Room TV",
            "a persisted name absent from the live list must survive the render, not reset"
        );
    }

    #[test]
    fn render_runtime_enum_on_a_non_string_field_labels_the_mismatch_and_does_not_panic() {
        let ctx = egui::Context::default();
        let mut value: u32 = 7;
        let snapshot: [crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry; 0] = [];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let field: &mut dyn bevy::reflect::PartialReflect = &mut value;
            render_runtime_enum(field, "s", "f", "k", &snapshot, ui);
        });
        // Reaching here without a panic is the assertion (mirrors the
        // existing `(unsupported number type)` / `(expected bool)` degrade
        // pattern the other render_* helpers use on a type mismatch).
    }
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p wc-core --lib settings::panel_user::widgets 2>&1 | head -20`

Expected: FAIL to compile — `cannot find function render_runtime_enum`.

- [ ] **Step 6: Implement `render_runtime_enum` and wire the `render_widget_value` match arm**

Add to `crates/wc-core/src/settings/panel_user/widgets.rs`, immediately after `set_enum_variant` (i.e. right before the `render_file_path` function):

```rust
/// Render the runtime-enumerated dropdown (`ComboBox`) for a field whose
/// candidate list is supplied by a registered
/// `crate::settings::RuntimeEnumOptionsSource` at render time, rather than
/// known at compile time (contrast `render_enum`). No label — Grid column 1
/// already holds it.
///
/// `runtime_enum_options` is the whole-panel snapshot taken once per frame in
/// `super::fields::render_section_by_key`, before the reflected field borrow
/// it needs `world` for makes `world` unavailable down here; `options_key`
/// selects this field's entry out of it via
/// `crate::settings::runtime_enum::options_for`.
///
/// The persisted value is never silently replaced:
/// `classify_runtime_enum_selection` decides whether it is in the live
/// list, and when it is not (source hasn't enumerated yet, device is
/// asleep/unplugged, or the name was typed by hand), it is still shown in the
/// `ComboBox` — marked "(unavailable)" — and stays selected. A `TextEdit`
/// alongside the `ComboBox` is the free-text escape hatch for exactly that
/// case: it lets the operator retype or correct the value directly instead of
/// waiting on enumeration.
fn render_runtime_enum(
    field: &mut dyn bevy::reflect::PartialReflect,
    storage_key: &'static str,
    field_name: &'static str,
    options_key: &'static str,
    runtime_enum_options: &[crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry],
    ui: &mut egui::Ui,
) {
    let Some(current) = field.try_downcast_mut::<String>() else {
        ui.label("(expected String)");
        return;
    };
    let options = crate::settings::runtime_enum::options_for(runtime_enum_options, options_key);
    let selection = classify_runtime_enum_selection(current.as_str(), options);
    let mut selected = current.clone();

    ui.horizontal(|ui| {
        let combo_label = match selection {
            RuntimeEnumSelection::Empty => "(none)".to_owned(),
            RuntimeEnumSelection::Known => current.clone(),
            RuntimeEnumSelection::Unavailable => format!("{current} (unavailable)"),
        };
        egui::ComboBox::from_id_salt(("wc-setting-runtime-enum", storage_key, field_name))
            .selected_text(combo_label)
            .show_ui(ui, |ui| {
                if selection == RuntimeEnumSelection::Unavailable {
                    let label = format!("{current} (unavailable)");
                    ui.selectable_value(&mut selected, current.clone(), label);
                }
                for opt in options {
                    ui.selectable_value(&mut selected, opt.clone(), opt.as_str());
                }
            });
        // Free-text escape hatch, always present.
        ui.add(egui::TextEdit::singleline(&mut selected).desired_width(120.0));
    });

    if selected != *current {
        *current = selected;
    }
}
```

Then update `render_widget_value`'s signature and match (currently lines 42-87):

```rust
pub(super) fn render_widget_value(
    field: &mut dyn bevy::reflect::PartialReflect,
    def: &SettingDef,
    storage_key: &'static str,
    runtime_enum_options: &[crate::settings::runtime_enum::RuntimeEnumOptionsSnapshotEntry],
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    #[cfg(feature = "templates")] template_dirty: &mut bool,
    #[cfg(feature = "templates")] style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
    match &def.kind {
        SettingKind::Number(range) => render_number(field, range, def.unit, ui),
        SettingKind::Boolean => render_bool(field, ui),
        SettingKind::Color => render_color(field, ui),
        SettingKind::Text => render_text(field, ui),
        SettingKind::TextList => render_text_list(field, ui),
        SettingKind::FilePath {
            filter_label,
            extensions,
        } => {
            render_file_path(field, filter_label, extensions, ui);
        }
        SettingKind::TemplateLibrary {
            filter_label,
            extensions,
        } => {
            #[cfg(feature = "templates")]
            super::template_picker::render_template_library(
                field,
                storage_key,
                def.field_name,
                filter_label,
                extensions,
                template_rows,
                template_dirty,
                style,
                ui,
            );
            // Permanent fallback when the `templates` feature is off.
            #[cfg(not(feature = "templates"))]
            render_file_path(field, filter_label, extensions, ui);
        }
        SettingKind::Enum { variants } => {
            render_enum(field, storage_key, def.field_name, variants, ui);
        }
        SettingKind::RuntimeEnum { options_key } => {
            render_runtime_enum(
                field,
                storage_key,
                def.field_name,
                options_key,
                runtime_enum_options,
                ui,
            );
        }
    }
}
```

(6 params without the `templates` feature, 8 with it — both at or under the workspace's `too-many-arguments-threshold = 8` in `clippy.toml`, so no lint attribute is needed here.)

- [ ] **Step 7: Run the widget tests to verify they pass**

Run: `cargo test -p wc-core --lib settings::panel_user::widgets`

Expected: PASS, including both new tests from Step 4.

- [ ] **Step 8: Thread the snapshot through `fields.rs`**

In `crates/wc-core/src/settings/panel_user/fields.rs`, add the import (alongside the existing `use` block near the top):

```rust
use crate::settings::runtime_enum::{self, RuntimeEnumOptionsSnapshotEntry};
```

In `render_section_by_key` (currently lines 33-105), insert the snapshot immediately after the existing "nothing to show" early return and before the `TypeRegistry` walk (i.e. right after the block ending at the current line 57, before the `let type_id = ...` at line 61):

```rust
    // Nothing to show when no field is visible at the current Advanced state
    // (e.g. a Dev-only struct while Advanced is off).
    if !defs.iter().any(|d| super::dock::field_visible(d, advanced)) {
        return;
    }

    // Snapshot every registered runtime-enum options source now, while
    // `world` is still a shared borrow -- once `reflect_mut` below is taken,
    // `world` is borrowed for the rest of this function and no widget below
    // can re-enter it. Mirrors the `defs` snapshot immediately above. See
    // `crate::settings::runtime_enum` for the registration side and this
    // plan's Task 3 design note for the one small per-frame allocation this
    // introduces.
    let runtime_enum_options = runtime_enum::snapshot(world);

    // Walk the type registry to find the settings type by its
    // SketchSettings::STORAGE_KEY. Compare by value, not pointer identity.
    let type_id = world
```

Then update the call to `render_user_fields_via_reflect` (currently the tail of `render_section_by_key`) to pass it through:

```rust
    render_user_fields_via_reflect(
        &mut *reflect_mut,
        defs.as_ref(),
        storage_key,
        provider_status,
        &runtime_enum_options,
        #[cfg(feature = "templates")]
        template_rows,
        #[cfg(feature = "templates")]
        template_dirty,
        default_instance.as_deref(),
        advanced,
        style,
        ui,
    );
```

Next, update `render_user_fields_via_reflect`'s signature (currently lines 138-149). It already carries `#[cfg_attr(feature = "templates", expect(clippy::too_many_arguments, reason = "..."))]` (lines 131-137) because the `templates`-feature param count exceeds `clippy.toml`'s `too-many-arguments-threshold = 8`; adding an always-present parameter now pushes the **non-templates** count over 8 too (9 without templates, 11 with), so the attribute must stop being feature-gated:

```rust
#[expect(
    clippy::too_many_arguments,
    reason = "the settings render chain threads the provider-status snapshot, the runtime-enum options snapshot, and (when the `templates` feature is on) the template rows + dirty flag through this fn; bundling them into a struct is a larger refactor out of scope here"
)]
fn render_user_fields_via_reflect(
    reflect: &mut dyn Reflect,
    defs: &[SettingDef],
    storage_key: &'static str,
    provider_status: Option<ProviderStatusLine>,
    runtime_enum_options: &[RuntimeEnumOptionsSnapshotEntry],
    #[cfg(feature = "templates")] template_rows: &[crate::templates::view::TemplateRow],
    #[cfg(feature = "templates")] template_dirty: &mut bool,
    default: Option<&dyn Reflect>,
    advanced: bool,
    style: &OverlayStyle,
    ui: &mut egui::Ui,
) {
```

And its call into `render_widget_value` (inside the `egui::Grid::new(...).show(ui, |ui| { for def in ... })` loop, currently around lines 207-219):

```rust
                    // Column 2: the value widget.
                    render_widget_value(
                        field,
                        def,
                        storage_key,
                        runtime_enum_options,
                        #[cfg(feature = "templates")]
                        template_rows,
                        #[cfg(feature = "templates")]
                        template_dirty,
                        #[cfg(feature = "templates")]
                        style,
                        ui,
                    );
```

- [ ] **Step 9: Remove Task 2's transient `dead_code` allows**

In `crates/wc-core/src/settings/panel_user/widgets.rs`, delete both `#[allow(dead_code, reason = "no non-test caller until alpha.5 Plan 03a Task 3 wires render_runtime_enum in")]` attributes added in Task 2 (one above `enum RuntimeEnumSelection`, one above `fn classify_runtime_enum_selection`). `render_runtime_enum` is now a real caller of both.

```bash
rg -n "no non-test caller until alpha.5 Plan 03a" crates/wc-core/src/settings/panel_user/widgets.rs   # expect: no matches after the edit
```

- [ ] **Step 10: Cosmetic doc update**

In `crates/wc-core/src/settings/panel_user/mod.rs`, the module doc's widget list (currently lines 40-42) reads:

```
//! - [`widgets`] — the typed value widgets (`Number`, `Boolean`, `Color`,
//!   `Text`, `Enum`, `FilePath`, plus the unreachable-for-now `Vec2`/`Vec3`
//!   branches).
```

Change to:

```
//! - [`widgets`] — the typed value widgets (`Number`, `Boolean`, `Color`,
//!   `Text`, `Enum`, `RuntimeEnum`, `FilePath`, plus the unreachable-for-now
//!   `Vec2`/`Vec3` branches).
```

- [ ] **Step 11: Run the full gate**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run -p wc-core --all-features
cargo test --doc -p wc-core
cargo doc --no-deps -p wc-core --document-private-items
```

Expected: all pass. If clippy reports `dead_code` on anything in `runtime_enum.rs`, a wiring step above is incomplete — fix the wiring, do not add an `#[allow]`.

- [ ] **Step 12: Commit**

```bash
git add crates/wc-core/src/settings/def.rs crates/wc-core/src/settings/commands.rs crates/wc-core/src/settings/panel_user/widgets.rs crates/wc-core/src/settings/panel_user/fields.rs crates/wc-core/src/settings/panel_user/mod.rs
```

Message file (`/tmp/wc-plan03a-task3.txt`):

```
feat(settings): add SettingKind::RuntimeEnum and its ComboBox widget

Adds the runtime-enumerated dropdown kind: a plain String field whose
ComboBox options come from a RuntimeEnumOptionsSource registered under
options_key (alpha.5 Plan 03a Task 1), resolved once per frame by
render_section_by_key before the reflected-field borrow makes world
unavailable to widgets. A persisted value absent from the live list is
never dropped -- render_runtime_enum marks it "(unavailable)" and
keeps it selected, with a free-text field alongside the ComboBox as
the escape hatch, per classify_runtime_enum_selection (Task 2).

Fixes both non-test production match sites the new SettingKind variant
breaks: commands.rs's console set-command write-back (folded into the
existing String-field arm) and widgets.rs's render_widget_value.
```

```bash
git commit -F /tmp/wc-plan03a-task3.txt
git show --stat HEAD
```

---

### Task 4: Derive-macro support — `ty = RuntimeEnum`

**Files:**
- Modify: `crates/wc-core-macros/src/lib.rs:1-483` (module doc, `Kind` enum, `FieldInfo`, `parse_setting_attr`, `expand`, new `validate_fields`, `kind_tokens`)
- Test: `crates/wc-core-macros/tests/derive.rs` (new fixture + tests)

**Interfaces:**
- Consumes: `SettingKind::RuntimeEnum { options_key }` (Task 3).
- Produces: `#[setting(ty = RuntimeEnum, options_key = "...")]` field attribute support; a compile error when `options_key` is missing on a `ty = RuntimeEnum` field.

Unlike `ty = Enum`'s variant list — which cannot be validated at macro-expansion time because a proc macro sees only the field's type name, not its definition, and so falls back to a runtime `debug_assert!` in `enum_variant_names` (`crates/wc-core/src/settings/def.rs:81-105`) — `options_key` is a literal string written directly in the `#[setting(...)]` attribute the macro is already parsing. The macro can and should check it at compile time. This task adds that check as a genuine improvement over the `Enum` precedent, not a copy of its runtime-only workaround.

- [ ] **Step 1: Write the failing test**

Add to `crates/wc-core-macros/tests/derive.rs`, after the existing `text_list_default_is_empty_and_roundtrips_toml` test (end of file):

```rust
/// Fixture exercising `ty = RuntimeEnum`: a `String` setting whose dropdown
/// options come from a runtime-registered source, not a compile-time enum
/// (contrast `Quality` / `ty = Enum` above).
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "derive_test_runtime_enum")]
struct RuntimeEnumFixture {
    #[setting(
        default = String::new(),
        ty = RuntimeEnum,
        options_key = "fixture_devices",
        label = "Device",
        category = User
    )]
    #[serde(default)]
    device: String,
}

#[test]
fn runtime_enum_kind_carries_options_key() {
    let defs = RuntimeEnumFixture::settings_def();
    let SettingKind::RuntimeEnum { options_key } = &defs[0].kind else {
        panic!("expected RuntimeEnum kind for device");
    };
    assert_eq!(*options_key, "fixture_devices");
    assert_eq!(defs[0].label, "Device");
}

#[test]
fn runtime_enum_default_is_empty_and_roundtrips_toml() {
    let d = RuntimeEnumFixture::default();
    assert!(d.device.is_empty());
    let with_device = RuntimeEnumFixture {
        device: "Living Room TV".to_owned(),
    };
    let toml_str = toml::to_string(&with_device).expect("serialize");
    let back: RuntimeEnumFixture = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(back, with_device);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p wc-core-macros 2>&1 | head -30`

Expected: FAIL to compile — the derive macro rejects `ty = RuntimeEnum` with `unknown ty \`RuntimeEnum\` (expected \`Number\`, \`Boolean\`, \`Color\`, \`Text\`, \`TextList\`, \`FilePath\`, \`TemplateLibrary\`, or \`Enum\`)"` and `options_key` with `unknown #[setting(...)] key`.

- [ ] **Step 3: Add the `Kind::RuntimeEnum` variant and the `options_key` field**

In `crates/wc-core-macros/src/lib.rs`, change the `Kind` enum (currently lines 104-120):

```rust
#[derive(Clone, Copy)]
enum Kind {
    Number,
    Boolean,
    Color,
    Text,
    /// Editable list of short strings, backed by a `Vec<String>` field.
    TextList,
    FilePath,
    /// Managed image template library; same `filter_label`/`extensions`
    /// attributes as `FilePath`, distinct `SettingKind`.
    TemplateLibrary,
    /// Unit-variant enum rendered as a `ComboBox`. Variant names are derived
    /// from the field type's reflection info at runtime, not listed in the
    /// attribute — see the module docs (`## ty = Enum`).
    Enum,
    /// `String`-valued `ComboBox` whose options come from a
    /// runtime-registered `RuntimeEnumOptionsSource`, not a Rust enum. See
    /// the module docs (`## ty = RuntimeEnum`).
    RuntimeEnum,
}
```

And `FieldInfo` (currently lines 122-143), adding one field after `filter_label`:

```rust
    /// File extensions for `Kind::FilePath`. None for other kinds.
    extensions: Option<Vec<String>>,
    /// Human-facing filter label for `Kind::FilePath`. None for other kinds.
    filter_label: Option<String>,
    /// Options-source key for `Kind::RuntimeEnum`. `None` for other kinds;
    /// required (checked in `validate_fields`) when `kind` is
    /// `Kind::RuntimeEnum`.
    options_key: Option<String>,
}
```

In `parse_fields`, the `FieldInfo { .. }` struct literal (currently around line 191-209) needs the new field initialised to `None`:

```rust
        let mut info = FieldInfo {
            ident,
            ty: field.ty.clone(),
            default: None,
            label: None,
            unit: None,
            section: None,
            category: Category::Dev,
            requires_restart: false,
            kind: default_kind_for_type(&field.ty),
            min: None,
            max: None,
            step: None,
            extensions: None,
            filter_label: None,
            options_key: None,
        };
```

- [ ] **Step 4: Parse the `options_key` attribute and the `RuntimeEnum` ty**

In `parse_setting_attr` (currently lines 225-302), add the `options_key` string arm after the existing `filter_label` arm:

```rust
    } else if meta.path.is_ident("filter_label") {
        let value: LitStr = meta.value()?.parse()?;
        info.filter_label = Some(value.value());
    } else if meta.path.is_ident("options_key") {
        let value: LitStr = meta.value()?.parse()?;
        info.options_key = Some(value.value());
    } else if meta.path.is_ident("category") {
```

And extend the `ty` match (currently lines 254-270):

```rust
    } else if meta.path.is_ident("ty") {
        let ident: Ident = meta.value()?.parse()?;
        info.kind = match ident.to_string().as_str() {
            "Number" => Kind::Number,
            "Boolean" => Kind::Boolean,
            "Color" => Kind::Color,
            "Text" => Kind::Text,
            "TextList" => Kind::TextList,
            "FilePath" => Kind::FilePath,
            "TemplateLibrary" => Kind::TemplateLibrary,
            "Enum" => Kind::Enum,
            "RuntimeEnum" => Kind::RuntimeEnum,
            other => {
                return Err(meta.error(format!(
                    "unknown ty `{other}` (expected `Number`, `Boolean`, `Color`, `Text`, `TextList`, `FilePath`, `TemplateLibrary`, `Enum`, or `RuntimeEnum`)"
                )))
            }
        };
```

- [ ] **Step 5: Add the compile-time `options_key` check**

In `crates/wc-core-macros/src/lib.rs`, change `expand` (currently lines 84-96) to call a new validation pass after parsing:

```rust
fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let struct_name = &input.ident;
    let storage_key = parse_storage_key(input)?;
    let fields = parse_fields(input)?;
    validate_fields(&fields)?;

    let default_impl = emit_default(struct_name, &fields);
    let trait_impl = emit_trait_impl(struct_name, &storage_key, &fields);

    Ok(quote! {
        #default_impl
        #trait_impl
    })
}
```

Add the new function directly below `parse_fields`:

```rust
/// Attribute combinations `parse_setting_attr` cannot reject on its own
/// because they depend on more than one attribute key at once --
/// `options_key` is only meaningful together with `ty = RuntimeEnum`, and
/// unlike `ty = Enum`'s variant-list contract (checked only at runtime; see
/// `enum_variant_names`'s docs in `wc_core::settings::def` for why), this one
/// the macro can and does check here, because `options_key` is a literal in
/// the attribute itself rather than something requiring the field type's own
/// definition.
fn validate_fields(fields: &[FieldInfo]) -> syn::Result<()> {
    for f in fields {
        if matches!(f.kind, Kind::RuntimeEnum) && f.options_key.is_none() {
            return Err(syn::Error::new(
                f.ident.span(),
                format!(
                    "`ty = RuntimeEnum` on `{}` requires `options_key = \"...\"`",
                    f.ident
                ),
            ));
        }
    }
    Ok(())
}
```

- [ ] **Step 6: Emit `SettingKind::RuntimeEnum` from `kind_tokens`**

In `emit_trait_impl`'s `kind_tokens` match (currently lines 382-446), add the new arm after `Kind::Enum`:

```rust
            Kind::Enum => {
                // Variant names come from the field type's reflection info at
                // runtime — `enum_variant_names` returns the `&'static` slice
                // baked into the enum's `TypeInfo`, and debug-asserts the
                // unit-variants-only contract (a proc macro cannot see the
                // enum definition, so this cannot be a compile error).
                let field_ty = &f.ty;
                quote! {
                    ::wc_core::settings::SettingKind::Enum {
                        variants: ::wc_core::settings::enum_variant_names::<#field_ty>(),
                    }
                }
            }
            Kind::RuntimeEnum => {
                // `validate_fields` already rejected a missing `options_key`
                // for this kind -- unlike `Kind::Enum`'s variant list, which
                // must be checked at runtime because a proc macro cannot see
                // the field type's own definition, `options_key` is a
                // literal in the attribute itself, so the macro checks it
                // here at compile time instead.
                let options_key = f
                    .options_key
                    .as_deref()
                    .expect("validate_fields already rejected a missing options_key");
                quote! {
                    ::wc_core::settings::SettingKind::RuntimeEnum {
                        options_key: #options_key,
                    }
                }
            }
```

(`.expect(...)` here is covered by the crate-wide `#![allow(clippy::expect_used, ...)]` at `lib.rs:64-67`.)

- [ ] **Step 7: Update the module doc**

In `crates/wc-core-macros/src/lib.rs`, update the attribute grammar table (currently lines 33-44):

```
//! | `ty`               | `Number` \| `Boolean` \| `Color` \| `Text` \| `TextList` \| `FilePath` \| `TemplateLibrary` \| `Enum` \| `RuntimeEnum` | `Number` |
//! | `min`, `max`, `step` | numeric expr | none (only meaningful on `Number`) |
//! | `extensions`       | `["ext", ...]` | none (only meaningful on `FilePath`) |
//! | `filter_label`     | string    | `"File"` (only meaningful on `FilePath`) |
//! | `options_key`      | string    | none (**required** on `RuntimeEnum`) |
//! | `requires_restart` | flag      | absent                           |
```

And add a new section immediately after the existing `## ty = Enum` section (after the current line 62):

```
//!
//! ## `ty = RuntimeEnum`
//!
//! The field's Rust type must be `String` (checked only at render time via
//! `try_downcast_mut`, exactly like `ty = Text` — the macro cannot verify
//! field types beyond the `bool` special-case in `default_kind_for_type`).
//! Unlike `ty = Enum`, whose variant list can only be checked at runtime (a
//! proc macro cannot see a field type's own definition), `options_key` is a
//! literal string in the attribute itself, so the macro checks it directly:
//! `ty = RuntimeEnum` without `options_key = "..."` is a **compile error**.
//! The live option list at that key comes from whichever
//! `wc_core::settings::RuntimeEnumOptionsSource` a module registers via
//! `wc_core::settings::RegisterRuntimeEnumOptionsExt::register_runtime_enum_options`
//! — see that trait's docs for the registration side. Use this instead of
//! `ty = Enum` whenever the candidate list is only known at runtime (an
//! enumerated audio device, a connected monitor), not fixed by a Rust enum.
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p wc-core-macros`

Expected: PASS, including the two new tests from Step 1 alongside all existing `derive.rs` tests.

- [ ] **Step 9: Run the scoped gate and commit**

```bash
cargo fmt --all
cargo clippy -p wc-core-macros --all-targets --all-features -- -D warnings
cargo clippy -p wc-core --all-targets --all-features -- -D warnings
cargo test -p wc-core-macros
git add crates/wc-core-macros/src/lib.rs crates/wc-core-macros/tests/derive.rs
```

Message file (`/tmp/wc-plan03a-task4.txt`):

```
feat(macros): add ty = RuntimeEnum to #[derive(SketchSettings)]

Emits SettingKind::RuntimeEnum { options_key } from a new
#[setting(ty = RuntimeEnum, options_key = "...")] field attribute.
Unlike ty = Enum's variant-list contract, which a proc macro can only
check at runtime because it cannot see the field type's own
definition, options_key is a literal in the attribute itself -- the
macro validates it is present at compile time and rejects a
ty = RuntimeEnum field missing it, rather than falling back to a
runtime debug_assert.
```

```bash
git commit -F /tmp/wc-plan03a-task4.txt
git show --stat HEAD
```

---

### Task 5: End-to-end proof and a worked example for Plans 03/04

**Files:**
- Modify: `crates/wc-core/src/settings/runtime_enum.rs` (append to the `#[cfg(test)] mod tests` block created in Task 1)

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces: nothing new consumed elsewhere — this task is verification only.

Tasks 1-4 are each individually tested, but nothing yet proves the whole chain a future plan will actually use: register a `RuntimeEnumOptionsSource`, derive a `SketchSettings` struct with a `ty = RuntimeEnum` field, and confirm the field's macro-emitted `options_key` resolves through the registry to that source's live list. This has to live inside `crates/wc-core/src/settings/runtime_enum.rs`'s own test module rather than an external `crates/wc-core/tests/*.rs` file or `wc-core-macros/tests/derive.rs`: `snapshot` and `options_for` are `pub(crate)`, invisible outside `wc-core`'s own source tree, and `wc-core` (unusually for a proc-macro pairing) depends on `wc-core-macros` as a normal — not dev-only — dependency (`crates/wc-core/Cargo.toml:81`, already exercised by `crates/wc-core/src/settings/hand_tracking.rs:5`), so a `#[derive(SketchSettings)]` fixture is available directly inside `wc-core`'s own `#[cfg(test)]` code.

- [ ] **Step 1: Write the test**

Append to the `#[cfg(test)] mod tests` block in `crates/wc-core/src/settings/runtime_enum.rs` (after the existing `options_for_an_unregistered_key_is_an_empty_slice` test), adding the two extra imports at the top of the `mod tests` block alongside the existing `use super::*;`:

```rust
mod tests {
    use super::*;
    use crate::settings::{SettingKind, SketchSettings};
    use serde::{Deserialize, Serialize};
    use wc_core_macros::SketchSettings;

    // ... existing FakeAudioDevices / FakeMonitors fixtures and tests above, unchanged ...

    /// The exact usage shape alpha.5 Plan 03 (monitor picker) and Plan 04
    /// (audio-device picker) will follow: a module registers its own
    /// `RuntimeEnumOptionsSource` resource, and a `#[derive(SketchSettings)]`
    /// struct elsewhere declares a `ty = RuntimeEnum` field with a matching
    /// `options_key`. Nothing here is aware of the other's existence beyond
    /// that shared string key.
    #[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
    #[reflect(Resource, Default)]
    #[settings(storage_key = "fixture_audio")]
    struct FixtureAudioSettings {
        #[setting(
            default = String::new(),
            ty = RuntimeEnum,
            options_key = "audio_output_devices",
            category = User,
            label = "Output device"
        )]
        #[serde(default)]
        output_device: String,
    }

    #[test]
    fn macro_generated_runtime_enum_field_resolves_through_the_registry() {
        let mut app = App::new();
        app.register_runtime_enum_options::<FakeAudioDevices>();
        app.insert_resource(FakeAudioDevices(vec![
            "Built-in Speakers".to_owned(),
            "HDMI TV".to_owned(),
        ]));

        let defs = FixtureAudioSettings::settings_def();
        let SettingKind::RuntimeEnum { options_key } = &defs[0].kind else {
            panic!("expected RuntimeEnum kind for output_device");
        };

        let snap = snapshot(app.world());
        assert_eq!(
            options_for(&snap, options_key).to_vec(),
            vec!["Built-in Speakers".to_owned(), "HDMI TV".to_owned()]
        );
    }
}
```

(`use crate::settings::{SettingKind, SketchSettings};` brings in the trait `SketchSettings` for `.settings_def()`, while `use wc_core_macros::SketchSettings;` brings in the derive macro of the same name — they occupy different namespaces (type vs. macro) and coexist without conflict; `crates/wc-core-macros/tests/derive.rs:18-19` already does exactly this in the sibling test crate.)

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p wc-core --lib settings::runtime_enum`

Expected: PASS, including the new `macro_generated_runtime_enum_field_resolves_through_the_registry` test alongside Task 1's four tests. This is a confirming test, not a red-green TDD cycle — by this point in the plan `SettingKind::RuntimeEnum`, the derive macro's `ty = RuntimeEnum`, and the registry all already exist and are each independently tested; this step only proves they cooperate the way Plans 03/04 will actually use them, so there is no "watch it fail first" step.

- [ ] **Step 3: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```

Expected: all pass.

- [ ] **Step 4: Manual smoke test**

Run: `cargo rund`

Expected: the app launches and the settings panel opens/closes normally (Shift+D or the cog button). No `RuntimeEnum` field exists in the live UI yet — this plan only adds the mechanism — so there is nothing new to see; the smoke test is confirming the settings panel as a whole still renders every existing `Enum`/`Text`/`FilePath`/`TemplateLibrary` field correctly with the now-larger `render_widget_value`/`render_user_fields_via_reflect` signatures.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/settings/runtime_enum.rs
```

Message file (`/tmp/wc-plan03a-task5.txt`):

```
test(settings): prove RuntimeEnum end-to-end via a macro-derived fixture

Ties Tasks 1-4 together with the exact shape alpha.5 Plan 03 (monitor
picker) and Plan 04 (audio-device picker) will use: an independently
registered RuntimeEnumOptionsSource resource, and a
#[derive(SketchSettings)] struct with a ty = RuntimeEnum field whose
options_key resolves through the registry to that source's live list.
Confirming test, not new behavior -- everything it exercises is
already covered by Tasks 1-4's own unit tests.
```

```bash
git commit -F /tmp/wc-plan03a-task5.txt
git show --stat HEAD
```

---

## Self-Review

**Every locked decision appears in a task.**
- "Options reach the widget via a Bevy `Resource` the panel reads... NOT via a function pointer stored in `SettingDef`" → Task 1 (`RuntimeEnumOptionsSource` + registry); `SettingDef`/`SettingKind` stay `'static` data (Task 3, Step 1) — the only function pointers added live in the separate `RuntimeEnumOptionsRegistry`, never in `SettingDef` itself.
- "When the persisted name is absent... show it, mark it unavailable, KEEP it persisted. Never silently rewrite." → `RuntimeEnumSelection::Unavailable` (Task 2) and `render_runtime_enum`'s `(unavailable)`-marked, still-selected `ComboBox` entry (Task 3, Step 6), unit-tested twice: the pure classifier (Task 2, Step 1) and the rendered widget (Task 3, Step 4).
- "There must be a free-text escape hatch" → the `TextEdit::singleline(&mut selected)` alongside the `ComboBox` in `render_runtime_enum` (Task 3, Step 6), plus the console `set` command folding `RuntimeEnum` into the same free-text write-back arm as `Text` (Task 3, Step 3).
- "The stored value is a `String`... persistence needs NO change" → `SettingKind::RuntimeEnum { options_key }` carries no value, only a key (Task 3, Step 1); the field itself stays a plain `String`, exercised by the TOML round-trip test in Task 4.

**Placeholder scan.** No `TODO`, `FIXME`, `unimplemented!()`, or "similar to Task N" shorthand anywhere in this plan's code blocks; every step shows complete code, including the full corrected `render_widget_value`, `render_user_fields_via_reflect`, and `kind_tokens` match after each edit (not diffs-only).

**Type/signature consistency between Produces and Consumes.**
- `RuntimeEnumOptionsSnapshotEntry` (Task 1, `pub(crate)` struct with `options_key: &'static str, options: Arc<[String]>`) is the exact type named in Task 3's `render_widget_value`, `render_user_fields_via_reflect`, and `render_runtime_enum` signatures, and in Task 5's fixture.
- `RuntimeEnumOptionsSnapshot = SmallVec<[RuntimeEnumOptionsSnapshotEntry; 4]>` (Task 1) is what `fields.rs`'s `runtime_enum::snapshot(world)` returns (Task 3, Step 8); callers pass `&runtime_enum_options`, which coerces to `&[RuntimeEnumOptionsSnapshotEntry]` via `SmallVec`'s `Deref<Target = [T]>`, the same coercion `Arc<[SettingDef]>` → `&[SettingDef]` already relies on one line above it.
- `options_for<'a>(snapshot: &'a [RuntimeEnumOptionsSnapshotEntry], options_key: &str) -> &'a [String]` (Task 1) is called identically in `render_runtime_enum` (Task 3) and the end-to-end test (Task 5).
- `classify_runtime_enum_selection(current: &str, options: &[String]) -> RuntimeEnumSelection` (Task 2) is called as `classify_runtime_enum_selection(current.as_str(), options)` in `render_runtime_enum` (Task 3), where `options: &[String]` is exactly `options_for`'s return type.
- `SettingKind::RuntimeEnum { options_key: &'static str }` (Task 3) is what the macro's `kind_tokens` (Task 4) emits, matched identically by `commands.rs` (Task 3, Step 3), `widgets.rs` (Task 3, Step 6), and the Task 4/5 tests via `let SettingKind::RuntimeEnum { options_key } = &defs[0].kind else { .. }`.

**Clippy-rule scan against this plan's own example code.** No `.expect()`/`.unwrap()` outside a `#[cfg(test)]` block or the `wc-core-macros` crate's pre-existing crate-wide `expect_used` allow. No `assert_eq!(x.is_some(), true)` (this plan's tests use `assert!(...)` / `assert_eq!` on concrete values throughout). No `0..(N + 1)` ranges (none of this plan's code constructs a range at all). No `Box::leak`. Every new `#[allow(dead_code, reason = "...")]` in Task 2 is transient with an explicit removal step named in Task 3.

**Complete list of `SettingKind` match/construction sites** (from `rg -n "SettingKind::" crates/`, re-verified against the working tree this plan was written from):

*Production, non-test, exhaustive — each needed a new arm, both fixed in Task 3:*
1. `crates/wc-core/src/settings/panel_user/widgets.rs:51` — `render_widget_value`'s `match &def.kind` (panel rendering dispatch).
2. `crates/wc-core/src/settings/commands.rs:63` — `set_setting`'s `match &def.kind` (console `set` write-back).

*Macro-internal, own `Kind` enum (not `SettingKind` itself, but the two sites that must gain the mirrored variant/arm for the new `ty` to exist at all), fixed in Task 4:*
3. `crates/wc-core-macros/src/lib.rs:104-120` — the `Kind` enum declaration.
4. `crates/wc-core-macros/src/lib.rs:254-270` — the `ty` string → `Kind` match (compile-error on unknown, not a silent wildcard).
5. `crates/wc-core-macros/src/lib.rs:382-446` — `kind_tokens`'s exhaustive match on `Kind`, emitting the actual `SettingKind::*` constructor tokens.

*Test-only or non-exhaustive — informational, not blocking, unaffected by this plan except where a fixture is added:*
6. `crates/wc-core-macros/tests/derive.rs` — per-field `let SettingKind::X(...) = ... else { panic!(...) }` assertions (Task 4 adds two more, following the exact existing style).
7. `crates/wc-core/src/settings/panel_user/widgets.rs:407-421` (`file_path_kind_dispatches`) and `crates/wc-sketches/src/line/settings.rs:847-852` — single-variant test assertions with a `_ => panic!` wildcard; untouched by this plan.
8. `crates/wc-core/src/settings/panel_user/dock.rs:220` — constructs a `SettingKind::Boolean` test fixture value, not a match; untouched.
9. `crates/wc-core/src/settings/panel_dev.rs` — zero references to `SettingKind`; the dev inspector uses `bevy-inspector-egui`'s generic reflection UI directly and needs no new arm.

**Ordering constraint.** Tasks 1 and 2 are independent of each other and of everything downstream (neither touches `SettingKind`), so they may be done in either order or in parallel by different reviewers, but both must land before Task 3. Task 3 must precede Task 4 (the macro emits `SettingKind::RuntimeEnum`, which must already exist) and Task 5 (which exercises both). Task 5 is last by construction — it is the only task that consumes all four others.

**Risk carried forward.** `render_section_by_key`'s new `runtime_enum::snapshot(world)` call (Task 3, Step 8) allocates a fresh `Arc<[String]>` per registered source on every frame the settings panel is open, not only when a source's data actually changes — documented and deliberately not fixed in this plan (see Task 3's design note); a future profiling pass on the settings panel should look here first if it ever shows up. Separately, this plan does not add a `trybuild`-style compile-fail test for the new `options_key`-required-on-`RuntimeEnum` macro error (Task 4, Step 5): the crate has no existing negative-compile-test infrastructure, and adding a new dev-dependency for one check runs against the project's general "avoid new dependencies" preference — the check itself is still real and covered by `validate_fields`, just not regression-tested by a compile-fail harness. Both are recorded here as open follow-ups, not silent gaps.
