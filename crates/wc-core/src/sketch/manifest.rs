//! Sketch metadata registry consumed by the Home-page picker.
//!
//! Each sketch's plugin calls
//! [`RegisterSketchManifestExt::register_sketch_manifest`] in its
//! `Plugin::build` to advertise its picker-tile metadata. The picker walks
//! [`crate::lifecycle::state::AppState::SKETCH_ORDER`] and looks each variant
//! up in the manifest — registered sketches render an active, clickable tile
//! with their screenshot; unregistered ones render a "Coming soon"
//! placeholder. This mirrors the `register_sketch_settings` pattern in
//! [`crate::settings::registry`] so adding a new sketch in Plan 12+ requires
//! zero changes to the picker.

use bevy::prelude::*;

use crate::lifecycle::state::AppState;

/// Picker-tile metadata for one sketch.
#[derive(Clone)]
pub struct SketchManifestEntry {
    /// Target state when the tile is clicked.
    pub state: AppState,
    /// Display name shown on the tile in Orbitron.
    pub display_name: &'static str,
    /// Storage key of this sketch's `SketchSettings` type (e.g. `"flame"`),
    /// matching its `#[settings(storage_key = "…")]`.
    ///
    /// This is the single source of truth that binds a sketch's `AppState` to
    /// its settings struct. The settings dock derives *both* which struct
    /// renders on the Sketch tab and that tab's label from here (see
    /// [`SketchManifest::settings_binding`] and
    /// [`SketchManifest::sketch_settings_keys`]), so registering a sketch's
    /// picker tile automatically wires its settings tab. Before this field the
    /// dock kept two hand-maintained `match` ladders that every ported sketch
    /// had to extend, and silently mis-showed the Line tab when one was missed.
    pub settings_key: &'static str,
    /// Tile background image. Loaded by the sketch's own plugin via
    /// `AssetServer` at startup.
    pub screenshot: Handle<Image>,
}

/// Lookup table of registered sketches. Inserted as a [`Resource`] by
/// [`crate::ui::WaveConductorUiPlugin`].
#[derive(Resource, Default)]
pub struct SketchManifest {
    entries: Vec<SketchManifestEntry>,
}

impl SketchManifest {
    /// Returns the registered entry for `state`, or `None` if no sketch
    /// plugin has registered itself for that variant.
    #[must_use]
    pub fn get(&self, state: AppState) -> Option<&SketchManifestEntry> {
        self.entries.iter().find(|e| e.state == state)
    }

    /// The `(settings_key, display_name)` of the sketch active in `state`, or
    /// `None` if no sketch is registered for it (e.g. `AppState::Home`).
    ///
    /// The settings dock's Sketch tab uses this to pick which settings struct
    /// renders and what label the tab shows, replacing a hardcoded
    /// `AppState -> (key, label)` match that silently fell back to Line.
    #[must_use]
    pub fn settings_binding(&self, state: AppState) -> Option<(&'static str, &'static str)> {
        self.get(state).map(|e| (e.settings_key, e.display_name))
    }

    /// Every registered sketch's settings storage key, in registration order.
    ///
    /// The settings dock uses this to route a settings struct to the Sketch tab
    /// (a key belongs to the Sketch tab iff it is some sketch's settings key),
    /// replacing a hardcoded `"line" | "dots" | …` match.
    pub fn sketch_settings_keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.iter().map(|e| e.settings_key)
    }

    /// Internal append used by [`RegisterSketchManifestExt`].
    pub(crate) fn push(&mut self, entry: SketchManifestEntry) {
        self.entries.push(entry);
    }
}

/// Load a sketch's picker-tile screenshot and register its manifest entry.
///
/// Collapses the boilerplate every sketch's `register_*_manifest` function
/// repeated verbatim: resolve the [`AssetServer`], kick off the async
/// screenshot load, and append the [`SketchManifestEntry`]. The load is async;
/// the picker renders the tile as soon as the image asset finishes loading,
/// showing the dark placeholder fill from `OverlayStyle` until then.
///
/// `settings_key` is the sketch's `SketchSettings` storage key (e.g. `"flame"`);
/// it binds this `AppState` to its settings struct so the dock's Sketch tab
/// resolves automatically (see [`SketchManifestEntry::settings_key`]).
///
/// `screenshot_path` is the asset-relative PNG path (e.g.
/// `"sketches/line/screenshot.png"`). Bevy's default features include the `png`
/// loader; JPEG would require the separate `bevy/jpeg` feature, which this
/// workspace does not enable.
pub fn register_sketch_tile(
    app: &mut App,
    state: AppState,
    display_name: &'static str,
    settings_key: &'static str,
    screenshot_path: &'static str,
) {
    let asset_server = app.world().resource::<AssetServer>();
    let screenshot = asset_server.load(screenshot_path);
    app.register_sketch_manifest(SketchManifestEntry {
        state,
        display_name,
        settings_key,
        screenshot,
    });
}

/// Extension trait — each sketch plugin's `build` calls this once.
pub trait RegisterSketchManifestExt {
    /// Append `entry` to the [`SketchManifest`]. Idempotent on duplicate
    /// `state` values: later entries silently overwrite earlier ones, which
    /// is the right behaviour for hot-reload scenarios.
    fn register_sketch_manifest(&mut self, entry: SketchManifestEntry) -> &mut Self;
}

impl RegisterSketchManifestExt for App {
    fn register_sketch_manifest(&mut self, entry: SketchManifestEntry) -> &mut Self {
        let world = self.world_mut();
        world.init_resource::<SketchManifest>();
        let mut manifest = world.resource_mut::<SketchManifest>();
        // Replace existing entry for the same state if one is present.
        if let Some(existing) = manifest.entries.iter_mut().find(|e| e.state == entry.state) {
            *existing = entry;
        } else {
            manifest.push(entry);
        }
        self
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions — panicking on unexpected None is the correct behaviour"
)]
mod tests {
    use super::*;

    fn dummy_entry(state: AppState, name: &'static str) -> SketchManifestEntry {
        SketchManifestEntry {
            state,
            display_name: name,
            settings_key: "dummy",
            screenshot: Handle::default(),
        }
    }

    #[test]
    fn get_returns_none_for_unregistered_state() {
        let manifest = SketchManifest::default();
        assert!(manifest.get(AppState::Line).is_none());
    }

    #[test]
    fn register_appends_entry_visible_via_get() {
        let mut app = App::new();
        app.register_sketch_manifest(dummy_entry(AppState::Line, "Line"));
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest
            .get(AppState::Line)
            .expect("Line entry should be registered");
        assert_eq!(entry.display_name, "Line");
    }

    #[test]
    fn duplicate_register_overwrites_entry() {
        let mut app = App::new();
        app.register_sketch_manifest(dummy_entry(AppState::Line, "Line"));
        app.register_sketch_manifest(dummy_entry(AppState::Line, "Line v2"));
        let manifest = app.world().resource::<SketchManifest>();
        let entry = manifest.get(AppState::Line).unwrap();
        assert_eq!(entry.display_name, "Line v2");
        assert_eq!(
            manifest.entries.len(),
            1,
            "duplicate state must not duplicate entries"
        );
    }

    /// `settings_binding` resolves an active state to its `(key, label)`; the
    /// dock derives the Sketch tab's contents and label from this, so a state
    /// with no registered sketch (Home) must return `None` rather than fall
    /// back to some other sketch's settings.
    #[test]
    fn settings_binding_resolves_key_and_label() {
        let mut manifest = SketchManifest::default();
        manifest.push(SketchManifestEntry {
            state: AppState::Line,
            display_name: "Gravity",
            settings_key: "line",
            screenshot: Handle::default(),
        });
        assert_eq!(
            manifest.settings_binding(AppState::Line),
            Some(("line", "Gravity"))
        );
        assert_eq!(
            manifest.settings_binding(AppState::Home),
            None,
            "an unregistered state must not borrow another sketch's settings"
        );
    }

    /// `sketch_settings_keys` yields every registered sketch's storage key; the
    /// dock uses membership in this set to route a key to the Sketch tab.
    #[test]
    fn sketch_settings_keys_lists_registered_keys() {
        let mut manifest = SketchManifest::default();
        manifest.push(SketchManifestEntry {
            state: AppState::Line,
            display_name: "Gravity",
            settings_key: "line",
            screenshot: Handle::default(),
        });
        manifest.push(SketchManifestEntry {
            state: AppState::Dots,
            display_name: "Fabric",
            settings_key: "dots",
            screenshot: Handle::default(),
        });
        let keys: Vec<&str> = manifest.sketch_settings_keys().collect();
        assert_eq!(keys, vec!["line", "dots"]);
    }
}
