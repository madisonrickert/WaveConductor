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
//! render time (see `panel_user::widgets::render_runtime_enum`).
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
//!
//! ## The string key is the whole contract, so debug builds check it
//!
//! Nothing in the type system ties a field's `options_key` literal to a
//! source's `OPTIONS_KEY` const — that decoupling is the point (neither side
//! names the other's type), but it means a typo on either side degrades into
//! an empty dropdown, which is *visually identical* to correctly-wired
//! hardware that is simply asleep or unplugged. `warn_on_unresolved_options_keys`
//! (debug builds only) cross-checks the two registries at startup and warns
//! rather than letting the author debug their hardware enumeration for a bug
//! that is a misspelled string.

use std::sync::Arc;

use bevy::prelude::*;

// Only the debug-build cross-check below reads the settings side of the
// contract; keep the imports off the release build so it stays warning-clean.
#[cfg(debug_assertions)]
use super::def::SettingKind;
#[cfg(debug_assertions)]
use super::registry::SettingsRegistry;

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
    /// dropdown. Called at most once per rendered section per registered
    /// source (see `snapshot`), so this should be a cheap field read — not a
    /// fresh OS enumeration call.
    fn options(&self) -> &[String];
}

/// One registered source's option list, captured by `snapshot` for the
/// section currently being rendered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RuntimeEnumOptionsSnapshotEntry {
    /// The source's `RuntimeEnumOptionsSource::OPTIONS_KEY`.
    pub(crate) options_key: &'static str,
    /// `RuntimeEnumOptionsSource::options` at snapshot time, ref-counted so
    /// cloning the snapshot list is a refcount bump, not a `Vec` copy.
    ///
    /// *Creating* the entry is not free, though: `snapshot_one` builds this
    /// `Arc` via `impl From<&[String]> for Arc<[String]>`, which allocates and
    /// **deep-copies every option `String`** — it cannot share the source
    /// resource's buffer, which the source owns. The refcount-bump economy
    /// applies only downstream of that one copy. This is why
    /// `panel_user::fields::render_section_by_key` takes a snapshot only for a
    /// section that actually declares a `SettingKind::RuntimeEnum` field, and
    /// why `RuntimeEnumOptionsSource::options` is specified as a cheap field
    /// read: the copy is per rendered section, per frame.
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
/// Called from `panel_user::fields::render_section_by_key` before the
/// reflected field borrow that needs `world`, which makes `world` unavailable
/// to the widgets that consume this snapshot — the same ordering constraint
/// the panel's own `SettingDef` list is read under.
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

/// Debug-build startup system: cross-check every declared `options_key`
/// against the registered sources, and warn on anything that cannot resolve.
///
/// Registered in `super::SettingsPlugin::build` under `Startup`, which runs
/// after every plugin's `build` has run — so both sides are fully populated
/// (`register_sketch_settings` and `register_runtime_enum_options` are both
/// `App`-build-time calls) and a warning here can never be an ordering
/// false positive.
///
/// Warns; never panics. A missing source is a real, shippable state at
/// *runtime* (the resource may simply not be inserted yet), and the panel
/// already degrades gracefully. What it must not be is *silent at startup*:
/// see the module docs for why an empty dropdown is indistinguishable from
/// absent hardware.
#[cfg(debug_assertions)]
pub(crate) fn warn_on_unresolved_options_keys(
    settings: Res<'_, SettingsRegistry>,
    sources: Res<'_, RuntimeEnumOptionsRegistry>,
) {
    for warning in options_key_warnings(&settings, &sources) {
        warn!("{warning}");
    }
}

/// The pure core of `warn_on_unresolved_options_keys`, returning one message
/// per problem found so it can be unit-tested without an `App`.
///
/// Two classes of problem, both silent failures at runtime:
///
/// 1. **Unresolved key** — a field declares `options_key = "x"` and no
///    registered source reports `OPTIONS_KEY == "x"`. `options_for` returns an
///    empty slice, so the dropdown is empty and the persisted value renders as
///    "(unavailable)".
/// 2. **Duplicate key** — two sources registered on one key.
///    `RegisterRuntimeEnumOptionsExt::register_runtime_enum_options` pushes
///    unconditionally and `options_for` takes the *first* match, so the second
///    source is silently shadowed and its options never appear.
///
/// Allocates freely: this runs once, at startup, in debug builds only.
#[cfg(debug_assertions)]
fn options_key_warnings(
    settings: &SettingsRegistry,
    sources: &RuntimeEnumOptionsRegistry,
) -> Vec<String> {
    let mut warnings = Vec::new();

    // (2) Duplicate OPTIONS_KEY. Report each entry that is shadowed by an
    // earlier one, so N registrations on a key yield N-1 warnings.
    for (idx, entry) in sources.entries.iter().enumerate() {
        if sources.entries[..idx]
            .iter()
            .any(|earlier| earlier.options_key == entry.options_key)
        {
            warnings.push(format!(
                "two `RuntimeEnumOptionsSource`s are registered under `options_key = \"{}\"`. \
                 `options_for` takes the first match, so this later registration is silently \
                 shadowed and its options will never appear. Give each source a distinct \
                 `OPTIONS_KEY`.",
                entry.options_key,
            ));
        }
    }

    // The keys that *are* available, listed in every warning so the author can
    // spot a typo without opening a single source file.
    let registered = if sources.entries.is_empty() {
        "(none)".to_owned()
    } else {
        format!(
            "\"{}\"",
            sources
                .entries
                .iter()
                .map(|e| e.options_key)
                .collect::<Vec<_>>()
                .join("\", \"")
        )
    };

    // (1) Unresolved options_key on a declared field.
    for registered_settings in &settings.entries {
        for def in registered_settings.def.iter() {
            let SettingKind::RuntimeEnum { options_key } = def.kind else {
                continue;
            };
            if sources
                .entries
                .iter()
                .any(|entry| entry.options_key == options_key)
            {
                continue;
            }
            warnings.push(format!(
                "settings field `{}.{}` declares `options_key = \"{}\"`, but no \
                 `RuntimeEnumOptionsSource` is registered under that key. Its dropdown will \
                 render empty and any persisted value will show as \"(unavailable)\" — \
                 indistinguishable from hardware that is merely absent. Registered keys: \
                 [{}]. Fix the `#[setting(options_key = ...)]` literal, or call \
                 `App::register_runtime_enum_options::<YourSource>()` for a source whose \
                 `OPTIONS_KEY` matches.",
                registered_settings.storage_key, def.field_name, options_key, registered,
            ));
        }
    }

    warnings
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test assertions; expect_used is denied workspace-wide for non-test code"
)]
mod tests {
    use super::*;
    use crate::settings::{SettingKind, SketchSettings};
    use serde::{Deserialize, Serialize};
    use wc_core_macros::SketchSettings;

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

    /// The exact usage shape alpha.5 Plan 03 (monitor picker) and Plan 04
    /// (audio-device picker) will follow: a module registers its own
    /// `RuntimeEnumOptionsSource` resource, and a `#[derive(SketchSettings)]`
    /// struct elsewhere declares a `ty = RuntimeEnum` field with a matching
    /// `options_key`. Nothing here is aware of the other's existence beyond
    /// that shared string key.
    #[derive(
        SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq,
    )]
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
    #[allow(
        clippy::panic,
        reason = "test assertion — panic on wrong variant is intentional"
    )]
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

    /// A second source deliberately colliding with `FakeAudioDevices`'
    /// `OPTIONS_KEY`, to exercise the duplicate-key branch of the debug check.
    #[derive(Resource, Default)]
    struct ShadowingAudioDevices(Vec<String>);

    impl RuntimeEnumOptionsSource for ShadowingAudioDevices {
        const OPTIONS_KEY: &'static str = "audio_output_devices";
        fn options(&self) -> &[String] {
            &self.0
        }
    }

    /// A `SettingsRegistry` holding just `FixtureAudioSettings`, whose single
    /// field declares `options_key = "audio_output_devices"`. Built by hand
    /// rather than via `register_sketch_settings` so the test touches neither
    /// the type registry nor the on-disk persistence path.
    #[cfg(debug_assertions)]
    fn fixture_settings_registry() -> SettingsRegistry {
        use crate::settings::registry::{
            diff_requires_restart_fn, is_changed_fn, save_fn, RegisteredSettings,
        };
        SettingsRegistry {
            entries: vec![RegisteredSettings {
                storage_key: FixtureAudioSettings::STORAGE_KEY,
                def: Arc::from(FixtureAudioSettings::settings_def()),
                save_fn: save_fn::<FixtureAudioSettings>,
                is_changed_fn: is_changed_fn::<FixtureAudioSettings>,
                diff_requires_restart_fn: diff_requires_restart_fn::<FixtureAudioSettings>,
            }],
        }
    }

    /// The options registry an `App` accumulated from its
    /// `register_runtime_enum_options` calls — the same path a real consumer
    /// takes, so the test checks what production actually builds.
    #[cfg(debug_assertions)]
    fn options_registry_of(app: &App) -> &RuntimeEnumOptionsRegistry {
        app.world().resource::<RuntimeEnumOptionsRegistry>()
    }

    /// The happy path: field key and source key agree, so nothing is reported.
    #[test]
    #[cfg(debug_assertions)]
    fn a_matching_options_key_produces_no_warning() {
        let mut app = App::new();
        app.register_runtime_enum_options::<FakeAudioDevices>();
        let warnings =
            options_key_warnings(&fixture_settings_registry(), options_registry_of(&app));
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    /// The bug this check exists for: the field says one thing, the source
    /// says another, and every runtime symptom looks like absent hardware.
    /// The warning must name the field, its storage key, and the keys that
    /// *are* registered.
    #[test]
    #[cfg(debug_assertions)]
    fn a_typod_options_key_is_reported_with_the_registered_keys() {
        let mut app = App::new();
        // Only a monitor source registered — the fixture field wants
        // "audio_output_devices".
        app.register_runtime_enum_options::<FakeMonitors>();
        let warnings =
            options_key_warnings(&fixture_settings_registry(), options_registry_of(&app));

        assert_eq!(warnings.len(), 1, "expected one warning, got {warnings:?}");
        let msg = &warnings[0];
        assert!(msg.contains("fixture_audio.output_device"), "{msg}");
        assert!(msg.contains("audio_output_devices"), "{msg}");
        assert!(msg.contains("display_monitors"), "{msg}");
    }

    /// With no sources registered at all, the warning still fires and says so
    /// explicitly rather than printing an empty list.
    #[test]
    #[cfg(debug_assertions)]
    fn an_options_key_with_no_sources_at_all_is_reported() {
        let warnings = options_key_warnings(
            &fixture_settings_registry(),
            &RuntimeEnumOptionsRegistry::default(),
        );
        assert_eq!(warnings.len(), 1, "expected one warning, got {warnings:?}");
        assert!(warnings[0].contains("(none)"), "{}", warnings[0]);
    }

    /// Two sources on one key: `options_for` takes the first, so the second is
    /// silently shadowed. Exactly one warning (the shadowed registration), and
    /// no unresolved-key warning — the key *does* resolve.
    #[test]
    #[cfg(debug_assertions)]
    fn a_duplicate_options_key_is_reported_as_shadowed() {
        let mut app = App::new();
        app.register_runtime_enum_options::<FakeAudioDevices>();
        app.register_runtime_enum_options::<ShadowingAudioDevices>();
        let warnings =
            options_key_warnings(&fixture_settings_registry(), options_registry_of(&app));

        assert_eq!(warnings.len(), 1, "expected one warning, got {warnings:?}");
        assert!(warnings[0].contains("shadowed"), "{}", warnings[0]);
        assert!(
            warnings[0].contains("audio_output_devices"),
            "{}",
            warnings[0]
        );
    }
}
