//! Bevy resources for the template library and the plugin that loads them.
//!
//! [`TemplateLibrary`] mirrors the on-disk manifest in memory (rebuilt on
//! import/delete); [`TemplateThumbnailCache`] holds session-lived egui texture
//! handles so thumbnails decode once. [`TemplatesPlugin`] loads + reconciles the
//! library at startup.

use std::collections::HashMap;
use std::path::PathBuf;

use bevy::prelude::*;
use bevy_egui::egui;

use crate::templates::manifest::TemplateEntry;
use crate::templates::{store, templates_dir};

/// In-memory view of the template manifest. `dir` is resolved once at startup so
/// every operation targets the same store. Rebuilt from disk via [`Self::reload`]
/// after any import or delete.
#[derive(Resource, Clone, Debug)]
pub struct TemplateLibrary {
    /// Absolute path to the managed templates directory.
    pub dir: PathBuf,
    /// Cached manifest entries (insertion order; the UI sorts a copy).
    pub entries: Vec<TemplateEntry>,
}

impl TemplateLibrary {
    /// Re-read the manifest from `dir` into `entries`.
    pub fn reload(&mut self) {
        self.entries = crate::templates::manifest::load_manifest(&self.dir).template;
    }
}

impl Default for TemplateLibrary {
    fn default() -> Self {
        Self {
            dir: templates_dir(),
            entries: Vec::new(),
        }
    }
}

/// Session-lived egui texture handles for thumbnails, keyed by content hash.
/// A handle kept here keeps its GPU texture alive; dropping it frees the texture.
#[derive(Resource, Default)]
pub struct TemplateThumbnailCache(pub HashMap<String, egui::TextureHandle>);

/// Loads + reconciles the template library at startup and registers its
/// resources. Inert without the panel (the cache only fills when the dock opens).
pub struct TemplatesPlugin;

impl Plugin for TemplatesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TemplateThumbnailCache>()
            .add_systems(Startup, load_library_on_startup);
    }
}

/// Startup system: resolve the store dir, prune dangling entries, and insert the
/// [`TemplateLibrary`] resource.
fn load_library_on_startup(mut commands: Commands<'_, '_>) {
    let dir = templates_dir();
    let entries = store::reconcile(&dir).template;
    commands.insert_resource(TemplateLibrary { dir, entries });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reload_reads_manifest_from_dir() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("a.png");
        // Reuse the store's test PNG writer via a tiny inline image.
        let mut img = image::RgbaImage::new(8, 8);
        for px in img.pixels_mut() {
            *px = image::Rgba([10, 20, 30, 255]);
        }
        img.save(&src).unwrap();
        crate::templates::store::ingest(dir.path(), &src).unwrap();

        let mut lib = TemplateLibrary {
            dir: dir.path().to_path_buf(),
            entries: Vec::new(),
        };
        lib.reload();

        assert_eq!(lib.entries.len(), 1);
    }
}
