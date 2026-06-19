//! Regression coverage for the template delete-clear persistence bug.
//!
//! Root cause was the `*v == row.managed_path` guard in
//! `panel_user::render_template_delete_confirm`: when the active path diverged
//! from the regenerated managed path (a raw source path from the file-picker
//! fallback, or any store/setting desync), bare string equality skipped
//! `v.clear()` and the dead path re-persisted ("file missing, using default"
//! next launch). These tests pin (1) that import and the delete comparison
//! produce the same managed path on clean data, and (2) the real
//! `store::active_ref_is_stale` the panel now calls — which clears on an exact
//! match OR a now-missing backing file, healing the divergent case.

#![cfg(feature = "templates")]
#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; serialized via a static mutex"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::items_after_statements,
    reason = "unwrap/expect and a local struct after statements are fine in test code"
)]

use std::sync::Mutex;

use wc_core::templates::resource::TemplateLibrary;
use wc_core::templates::view::build_rows;
use wc_core::templates::{store, templates_dir, DATA_DIR_ENV};

/// Serialize env-var mutation (process-global) across tests in this binary.
fn with_data_dir<R>(f: impl FnOnce() -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().expect("env mutex");
    let dir = tempfile::TempDir::new().expect("tempdir");
    let prev = std::env::var_os(DATA_DIR_ENV);
    // SAFETY: serialized by LOCK above.
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
    }
    let r = f();
    // SAFETY: same lock.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(DATA_DIR_ENV, v),
            None => std::env::remove_var(DATA_DIR_ENV),
        }
    }
    drop(dir);
    r
}

/// Write a tiny valid PNG `decode`s succeed on.
fn write_png(path: &std::path::Path) {
    let mut img = image::RgbaImage::new(8, 8);
    for px in img.pixels_mut() {
        *px = image::Rgba([10, 20, 30, 255]);
    }
    img.save(path).unwrap();
}

/// Mirrors the panel exactly:
/// - IMPORT sets `v = store::managed_path(&templates_dir(), &entry)…`.
/// - DELETE compares `v == row.managed_path`, where the row is built by
///   `build_rows(&TemplateLibrary{ dir: templates_dir(), … })`.
///
/// If these two strings differ, `v.clear()` never runs and the bug is (a).
#[test]
fn import_path_matches_delete_comparison_path() {
    with_data_dir(|| {
        let dir = templates_dir();
        let src = dir.parent().unwrap().join("source.png");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_png(&src);

        // IMPORT: ingest + what the panel assigns into the field.
        let entry = store::ingest(&dir, &src).expect("ingest");
        let import_v = store::managed_path(&dir, &entry)
            .to_string_lossy()
            .into_owned();

        // STARTUP/RELOAD: library resource as `load_library_on_startup` builds it.
        let lib = TemplateLibrary {
            dir: templates_dir(),
            entries: store::reconcile(&templates_dir()).template,
        };
        let rows = build_rows(&lib);
        let row = rows
            .iter()
            .find(|r| r.hash == entry.hash)
            .expect("imported entry must appear as a row");

        assert_eq!(
            import_v, row.managed_path,
            "import-set path != delete-comparison path; v.clear() guard would be false"
        );
    });
}

/// Pins the real `store::active_ref_is_stale` the delete handler now calls. The
/// active template reference is cleared when it matches the deleted blob OR its
/// backing file is gone; a divergent-but-still-valid path is left intact, and an
/// empty reference is never stale. Case (3) is the fix for the reported bug: a
/// dead source path (the original deleted off disk) is healed on delete instead
/// of surviving to warn "file missing" on the next launch.
#[test]
fn active_ref_is_stale_clears_dead_or_matching_paths() {
    with_data_dir(|| {
        let dir = templates_dir();
        let src = dir.parent().unwrap().join("photo.png");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_png(&src);

        let entry = store::ingest(&dir, &src).expect("ingest");
        let lib = TemplateLibrary {
            dir: templates_dir(),
            entries: store::reconcile(&templates_dir()).template,
        };
        let rows = build_rows(&lib);
        let row = rows.iter().find(|r| r.hash == entry.hash).unwrap();
        let managed = row.managed_path.clone();
        let src_str = src.to_string_lossy().into_owned();
        assert_ne!(
            src_str, managed,
            "precondition: source path differs from managed path"
        );

        // (1) Active path IS the deleted managed blob -> stale -> cleared.
        assert!(
            store::active_ref_is_stale(&managed, &managed),
            "the managed active path must clear on its own delete"
        );

        // (2) Divergent active path whose file still exists -> NOT stale ->
        // left intact (don't clear a valid, unrelated reference).
        assert!(
            !store::active_ref_is_stale(&src_str, &managed),
            "a divergent but existing active path must be left intact"
        );

        // (3) Divergent active path whose backing file is GONE (the reported
        // bug: a dead source path) -> stale -> cleared. This is the fix.
        std::fs::remove_file(&src).unwrap();
        assert!(
            store::active_ref_is_stale(&src_str, &managed),
            "a divergent dead active path must be cleared (heals the stale path)"
        );

        // (4) No active template -> nothing to clear.
        assert!(
            !store::active_ref_is_stale("", &managed),
            "empty active path is never stale"
        );
    });
}

/// Cross-session variant: the imported path is persisted as a TOML string and
/// reloaded (as `spawn_template` would be on next launch), then compared against
/// a freshly-computed `managed_path`. Confirms a relaunch does not perturb the
/// string the guard compares.
#[test]
fn persisted_path_matches_delete_comparison_after_reload() {
    with_data_dir(|| {
        let dir = templates_dir();
        let src = dir.parent().unwrap().join("source.png");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        write_png(&src);

        let entry = store::ingest(&dir, &src).expect("ingest");
        let import_v = store::managed_path(&dir, &entry)
            .to_string_lossy()
            .into_owned();

        // Round-trip the active path through TOML (session 1 save → session 2 load).
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Holder {
            spawn_template: String,
        }
        let toml_text = toml::to_string(&Holder {
            spawn_template: import_v.clone(),
        })
        .unwrap();
        let reloaded: Holder = toml::from_str(&toml_text).unwrap();

        let lib = TemplateLibrary {
            dir: templates_dir(),
            entries: store::reconcile(&templates_dir()).template,
        };
        let rows = build_rows(&lib);
        let row = rows.iter().find(|r| r.hash == entry.hash).unwrap();

        assert_eq!(
            reloaded.spawn_template, row.managed_path,
            "persisted path != delete-comparison path after reload"
        );
    });
}
