//! Pure (egui-free at the data level) view-model for the template-library
//! widget: turns manifest entries into display rows. Thumbnail texture ids are
//! attached later by the panel (see `settings::panel_user`), so the row-building
//! logic stays unit-testable.

use bevy_egui::egui;

use crate::templates::resource::TemplateLibrary;
use crate::templates::store::managed_path;

/// One row in the template dropdown.
#[derive(Clone, Debug)]
pub struct TemplateRow {
    /// Content hash (stable id for selection, delete, thumbnail lookup).
    pub hash: String,
    /// Friendly label (the original filename).
    pub label: String,
    /// `"WxH · <size>"` subtext.
    pub subtext: String,
    /// Absolute path to the managed blob, written into the setting on select.
    pub managed_path: String,
    /// Thumbnail texture id, filled by the panel once decoded; `None` until then.
    pub thumb: Option<egui::TextureId>,
}

/// Build display rows from the library, sorted newest-import first. The
/// thumbnail id is left `None` here; the panel attaches it after decoding.
#[must_use]
pub fn build_rows(lib: &TemplateLibrary) -> Vec<TemplateRow> {
    let mut entries: Vec<_> = lib.entries.iter().collect();
    entries.sort_by(|a, b| b.imported_at.cmp(&a.imported_at));
    entries
        .into_iter()
        .map(|e| TemplateRow {
            hash: e.hash.clone(),
            label: e.original_name.clone(),
            subtext: format!("{}x{} · {}", e.width, e.height, human_bytes(e.bytes)),
            managed_path: managed_path(&lib.dir, e).to_string_lossy().into_owned(),
            thumb: None,
        })
        .collect()
}

/// Human-readable byte size (`"512 B"`, `"471 KB"`, `"5.0 MB"`).
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if n < KB {
        format!("{n} B")
    } else if n < MB {
        // Round to nearest KB (not truncate): 482_133 B → 471 KB, not 470.
        format!("{} KB", (n + KB / 2) / KB)
    } else {
        // u64 -> f64 is lossy only above 2^53 bytes (8 PB); irrelevant for images.
        format!("{:.1} MB", n as f64 / MB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::manifest::TemplateEntry;

    fn entry(hash: &str, name: &str, at: u64, w: u32, h: u32, bytes: u64) -> TemplateEntry {
        TemplateEntry {
            hash: hash.into(),
            ext: "png".into(),
            original_name: name.into(),
            imported_at: at,
            width: w,
            height: h,
            bytes,
            thumb: format!("thumbs/{hash}.png"),
        }
    }

    #[test]
    fn rows_sort_newest_first_with_subtext() {
        let lib = TemplateLibrary {
            dir: std::path::PathBuf::from("/store"),
            entries: vec![
                entry("old", "old.png", 100, 640, 480, 1024),
                entry("new", "new.png", 200, 1280, 853, 482_133),
            ],
        };
        let rows = build_rows(&lib);
        assert_eq!(rows[0].label, "new.png");
        assert_eq!(rows[0].subtext, "1280x853 · 471 KB");
        assert_eq!(rows[1].label, "old.png");
        assert_eq!(rows[0].managed_path, "/store/new.png");
    }

    #[test]
    fn human_bytes_scales() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(482_133), "471 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
