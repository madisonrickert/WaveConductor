//! Per-image template adjustments, persisted as a registered settings resource.
//!
//! A hash-keyed map of [`TemplateAdjustments`] (one entry per cached image). It
//! rides the existing settings persistence/autosave: it implements
//! [`SketchSettings`] with an **empty** `settings_def` (so the reflection panel
//! draws nothing for it — its UI is the custom "Template adjustments" dock
//! section), which means the `HashMap` field serializes through the central
//! `sketch-settings.toml` and editing the map via `world.get_resource_mut` arms
//! the existing debounce. No separate file, no separate flush machinery.

use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use wc_core::settings::{SettingDef, SketchSettings};

use crate::line::template_adjustments::TemplateAdjustments;

/// Per-image adjustments keyed by the image's content hash (the managed blob's
/// file stem). Registered like any settings type so it persists/autosaves
/// centrally; rendered by the custom dock section, not the reflection panel.
#[derive(Resource, Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[reflect(Resource, Default)]
pub struct LineTemplateAdjustments {
    /// `content hash → adjustments`. An absent hash means identity (default)
    /// adjustments — entries are written lazily on first edit.
    pub map: HashMap<String, TemplateAdjustments>,
}

impl SketchSettings for LineTemplateAdjustments {
    const STORAGE_KEY: &'static str = "line-template-adjustments";

    /// Empty: this type carries no flat fields the reflection panel can render;
    /// its UI is the custom "Template adjustments" dock section.
    fn settings_def() -> Vec<SettingDef> {
        Vec::new()
    }
}

impl LineTemplateAdjustments {
    /// The active image's adjustments (cloned), or
    /// [`TemplateAdjustments::default`] when the hash has no saved entry.
    #[must_use]
    pub fn get(&self, hash: &str) -> TemplateAdjustments {
        self.map.get(hash).cloned().unwrap_or_default()
    }

    /// Mutable access to the active image's adjustments, inserting the identity
    /// default on first touch.
    pub fn entry_mut(&mut self, hash: &str) -> &mut TemplateAdjustments {
        self.map.entry(hash.to_owned()).or_default()
    }

    /// The active template's colour influence (`0..1`), or `0.0` when no
    /// template is active or it has no saved entry. Allocation-free — used by
    /// the per-frame colour-influence driver, so it must not allocate (the
    /// no-hot-path-allocation rule in AGENTS.md).
    #[must_use]
    pub fn color_influence_for(&self, spawn_template: &str) -> f32 {
        hash_of_path_str(spawn_template)
            .and_then(|h| self.map.get(h))
            .map_or(0.0, |a| a.color_influence)
    }
}

/// The content hash for an active `spawn_template` path, **borrowed** from the
/// path: the managed blob's file stem (blobs are stored as `{hash}.{ext}`).
/// `None` for an empty path. Allocation-free for per-frame callers.
#[must_use]
pub fn hash_of_path_str(spawn_template: &str) -> Option<&str> {
    if spawn_template.is_empty() {
        return None;
    }
    Path::new(spawn_template)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
}

/// Owned variant of [`hash_of_path_str`] for cold callers that need a `String`
/// key (the panel's `entry_mut`, spawn). Per-frame callers use the borrowing
/// form to avoid allocating.
#[must_use]
pub fn hash_of_path(spawn_template: &str) -> Option<String> {
    hash_of_path_str(spawn_template).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_of_path_extracts_stem() {
        assert_eq!(
            hash_of_path("/data/waveconductor/templates/deadbeef.png").as_deref(),
            Some("deadbeef")
        );
        assert_eq!(hash_of_path(""), None);
    }

    #[test]
    fn get_returns_default_for_unknown_hash() {
        let store = LineTemplateAdjustments::default();
        assert_eq!(store.get("nope"), TemplateAdjustments::default());
    }

    #[test]
    fn entry_mut_inserts_default_then_persists_edits() {
        let mut store = LineTemplateAdjustments::default();
        store.entry_mut("h").gamma = 2.0;
        assert!((store.get("h").gamma - 2.0).abs() < 1e-6);
        // A second touch returns the same (now non-default) entry.
        assert!((store.entry_mut("h").gamma - 2.0).abs() < 1e-6);
    }
}
