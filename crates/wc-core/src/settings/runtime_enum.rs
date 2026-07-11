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
pub(crate) type RuntimeEnumOptionsSnapshot =
    smallvec::SmallVec<[RuntimeEnumOptionsSnapshotEntry; 4]>;

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
/// of this plan) before the reflected field borrow that needs `world`, which
/// makes `world` unavailable to the widgets that consume this snapshot — the
/// same ordering constraint the panel's own `SettingDef` list is read under.
///
/// `world.get_resource::<RuntimeEnumOptionsRegistry>()` and each entry's
/// `snapshot_fn(world)` are both shared (`&World`) borrows, so — unlike
/// `super::registry::emit_restart_events`, which snapshots its fn-pointer
/// list *before* re-entering `world` because its per-type functions need
/// `&mut World` — this reads the registry and calls every entry's
/// `snapshot_fn` in one pass without a two-phase snapshot.
// Dead in the lib target until Task 3 of this plan wires
// `panel_user::fields::render_section_by_key` to call it. Three deliberate
// choices here, mirrored on `options_for` below:
//   - `expect`, not `allow`, so `unfulfilled_lint_expectations` becomes a
//     hard error under `-D warnings` the moment Task 3 supplies the real
//     caller. A bare `allow` would sit here forever AND would silently
//     defeat Task 3's own wiring-completeness check, which is precisely
//     "does clippy still report dead_code in this module?".
//   - `cfg_attr(not(test), ...)`, because `--all-targets` also builds the
//     lib *test* target, where `mod tests` does call this — there the
//     expectation would be unfulfilled and would fail the gate by itself.
//   - the `reason` names the task that removes it.
// The three types above need no attribute of their own: an expect/allow on a
// fn makes it a live root for dead-code analysis, so everything it names
// (`RuntimeEnumOptionsSnapshot`, `RuntimeEnumOptionsSnapshotEntry`,
// `RuntimeEnumOptionsRegistryEntry`) is reachable through these two.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no non-test caller until alpha.5 Plan 03a Task 3 wires render_runtime_enum in"
    )
)]
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
// The widget call site (Task 3 of this plan) uses this; the lib target sees
// it as dead until then.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "no non-test caller until alpha.5 Plan 03a Task 3 wires render_runtime_enum in"
    )
)]
pub(crate) fn options_for<'a>(
    snapshot: &'a [RuntimeEnumOptionsSnapshotEntry],
    options_key: &str,
) -> &'a [String] {
    snapshot
        .iter()
        .find(|entry| entry.options_key == options_key)
        .map_or(&[], |entry| entry.options.as_ref())
}

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
