//! Type registry: lets `SettingsPlugin` orchestrate save / restart logic
//! over a heterogeneous list of `SketchSettings` types.
//!
//! Each registered type contributes one entry of type-erased function
//! pointers. The panels and persistence systems iterate the list and call
//! through the pointers without knowing the concrete type.

use bevy::prelude::*;
use bevy::reflect::{FromType, GetTypeRegistration, TypePath};

use super::def::SettingDef;
use super::event::SketchRestart;
use super::persistence;
use super::trait_def::SketchSettings;

/// Per-registered-type entry stored in [`SettingsRegistry`].
#[derive(Clone)]
pub struct RegisteredSettings {
    /// `S::STORAGE_KEY` — used as the toml table name / localStorage suffix
    /// and as the discriminator on `SketchRestart` messages.
    pub storage_key: &'static str,
    /// Cached `S::settings_def()` so panel renderers don't reallocate per
    /// frame.
    pub def: Vec<SettingDef>,
    /// Persist the current value of the registered resource by reading it
    /// from `world` and calling `persistence::save`.
    pub save_fn: fn(&World),
    /// Returns `true` if the registered resource has been mutated this frame.
    /// Used by the debounced auto-save system in [`super::autosave`].
    pub is_changed_fn: fn(&World) -> bool,
    /// Returns true if any `requires_restart` field changed value since the
    /// previous frame. Implementation maintains a per-type `Local`-style
    /// last-seen snapshot inside a hidden resource (`PreviousSnapshot<S>`).
    pub diff_requires_restart_fn: fn(&mut World) -> bool,
}

/// Heterogeneous, type-erased list of registered settings types.
///
/// Populated by [`super::RegisterSketchSettingsExt::register_sketch_settings`].
#[derive(Resource, Default, Clone)]
pub struct SettingsRegistry {
    /// One entry per registered settings type, in registration order.
    pub entries: Vec<RegisteredSettings>,
}

/// Hidden resource: previous-frame snapshot of each settings type.
///
/// Used by the requires-restart diff function. Stored separately per `S`.
#[derive(Resource, Debug, Clone)]
pub struct PreviousSnapshot<S: SketchSettings>(pub S);

impl<S: SketchSettings> Default for PreviousSnapshot<S> {
    fn default() -> Self {
        Self(S::default())
    }
}

/// Returns `true` if any field marked `requires_restart` differs between
/// `prev` and `curr`. Compares by serializing both to TOML values — slower
/// than per-field equality but works without a per-struct generated diff
/// function. Only called after confirming `S` was mutated this frame.
fn requires_restart_changed<S: SketchSettings>(prev: &S, curr: &S) -> bool {
    let restart_fields: Vec<&'static str> = S::settings_def()
        .iter()
        .filter(|d| d.requires_restart)
        .map(|d| d.field_name)
        .collect();
    if restart_fields.is_empty() {
        return false;
    }
    let prev_v = toml::Value::try_from(prev).ok();
    let curr_v = toml::Value::try_from(curr).ok();
    let (Some(prev_v), Some(curr_v)) = (prev_v, curr_v) else {
        return false;
    };
    for name in restart_fields {
        if prev_v.get(name) != curr_v.get(name) {
            return true;
        }
    }
    false
}

/// The save closure baked per `S` at registration time.
pub fn save_fn<S: SketchSettings>(world: &World) {
    let value = world.resource::<S>().clone();
    persistence::save::<S>(&value);
}

/// Returns `true` when the `S` resource has been mutated since the last tick.
///
/// Used by the debounced auto-save system to arm the per-type timer without
/// cloning the full settings value.
pub fn is_changed_fn<S: SketchSettings>(world: &World) -> bool {
    world.is_resource_changed::<S>()
}

/// The restart-diff closure baked per `S` at registration time.
///
/// Short-circuits when `S` has not been mutated this frame — no cloning,
/// no snapshot update, no TOML serialization. Only when `is_resource_changed`
/// returns true does it clone the resource, update `PreviousSnapshot<S>`,
/// and delegate to `requires_restart_changed`.
pub fn diff_requires_restart_fn<S: SketchSettings>(world: &mut World) -> bool {
    if !world.is_resource_changed::<S>() {
        return false;
    }
    let curr = world.resource::<S>().clone();
    // PreviousSnapshot<S> is inserted at registration time; if it's missing
    // here, S was not registered through register_sketch_settings — return
    // false rather than panicking.
    let prev_snap = world
        .get_resource_mut::<PreviousSnapshot<S>>()
        .map(|mut p| {
            let old = p.0.clone();
            p.0 = curr.clone();
            old
        });
    let Some(prev) = prev_snap else {
        return false;
    };
    requires_restart_changed::<S>(&prev, &curr)
}

/// Extension trait that adds a typed `register_sketch_settings::<S>` method
/// to Bevy's [`App`].
pub trait RegisterSketchSettingsExt {
    /// Register a [`SketchSettings`] type with the settings system.
    ///
    /// Loads any persisted value (else default), inserts it as a resource,
    /// records type metadata in [`SettingsRegistry`], and seeds a
    /// [`PreviousSnapshot`] so restart-diffing has a baseline.
    fn register_sketch_settings<S: SketchSettings + GetTypeRegistration + TypePath>(
        &mut self,
    ) -> &mut Self;
}

impl RegisterSketchSettingsExt for App {
    fn register_sketch_settings<S: SketchSettings + GetTypeRegistration + TypePath>(
        &mut self,
    ) -> &mut Self {
        let initial = persistence::load::<S>();
        self.insert_resource(initial.clone());
        self.insert_resource(PreviousSnapshot::<S>(initial));
        self.register_type::<S>();
        self.register_type_data::<S, SettingsTypeKey>();

        let mut registry = self
            .world_mut()
            .get_resource_or_insert_with(SettingsRegistry::default)
            .clone();
        registry.entries.push(RegisteredSettings {
            storage_key: S::STORAGE_KEY,
            def: S::settings_def(),
            save_fn: save_fn::<S>,
            is_changed_fn: is_changed_fn::<S>,
            diff_requires_restart_fn: diff_requires_restart_fn::<S>,
        });
        self.insert_resource(registry);
        self
    }
}

/// Inline stack snapshot type for `emit_restart_events`.
///
/// Holds `(diff_fn, storage_key)` pairs without allocating for ≤8 types.
type RestartSnapshot = smallvec::SmallVec<[(fn(&mut World) -> bool, &'static str); 8]>;

/// System that, once per frame, walks the registry calling each entry's
/// `diff_requires_restart_fn` and emits a [`SketchRestart`] if any returned
/// true.
///
/// Snapshots only the function pointer + storage key for each entry into a
/// stack `SmallVec` so the registry resource stays unborrowed while the
/// per-type functions reborrow the world. No per-frame `Vec<SettingDef>`
/// clone; the `def` field is left untouched.
pub fn emit_restart_events(world: &mut World) {
    // Snapshot the (fn, key) pairs. Most apps will register ≤8 settings types.
    let snapshot: RestartSnapshot = world
        .get_resource::<SettingsRegistry>()
        .map(|r| {
            r.entries
                .iter()
                .map(|e| (e.diff_requires_restart_fn, e.storage_key))
                .collect()
        })
        .unwrap_or_default();
    for (diff_fn, storage_key) in snapshot {
        if diff_fn(world) {
            world
                .resource_mut::<bevy::prelude::Messages<SketchRestart>>()
                .write(SketchRestart { storage_key });
        }
    }
}

/// Reflect type-data that tags a settings type with its
/// [`super::trait_def::SketchSettings::STORAGE_KEY`].
///
/// Inserted at registration time by [`RegisterSketchSettingsExt`]. The
/// reflection-driven user panel walker (the private `panel_user` module) looks
/// up types by this key.
#[derive(Clone, Debug)]
pub struct SettingsTypeKey(pub &'static str);

impl<T: SketchSettings> FromType<T> for SettingsTypeKey {
    fn from_type() -> Self {
        Self(T::STORAGE_KEY)
    }
}
