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

    /// Internal append used by [`RegisterSketchManifestExt`].
    pub(crate) fn push(&mut self, entry: SketchManifestEntry) {
        self.entries.push(entry);
    }
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
        assert_eq!(manifest.entries.len(), 1, "duplicate state must not duplicate entries");
    }
}
