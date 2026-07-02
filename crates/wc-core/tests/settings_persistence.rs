//! Round-trip and resilience tests for the TOML persistence layer.

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; serialized below via a static mutex"
)]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]
#![allow(
    clippy::field_reassign_with_default,
    reason = "test code explicitly mutates fields after default construction for clarity"
)]

use tempfile::TempDir;
use wc_core::settings::{
    persistence::{self, CONFIG_DIR_ENV},
    SketchSettings,
};

mod common;
use common::{TestBlendMode, TestSketchSettings};

/// Set `CONFIG_DIR_ENV` to `dir` for the duration of the closure.
///
/// Tests in this file are serial (the env var is process-global). Run with
/// `cargo test -p wc-core --test settings_persistence -- --test-threads=1`
/// or rely on cargo's per-binary default of a single thread when the
/// `RUST_TEST_THREADS` env var is set elsewhere. We enforce it via an
/// in-process mutex below.
fn with_temp_dir<R>(f: impl FnOnce(&TempDir) -> R) -> R {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().expect("env-var mutex poisoned");
    let dir = TempDir::new().expect("tempdir");
    let prev = std::env::var_os(CONFIG_DIR_ENV);
    // SAFETY: serialized via the static `LOCK` mutex above, so no other
    // thread can observe or mutate environment variables while this block
    // runs. Rust 1.80+ marks `set_var`/`remove_var` unsafe specifically to
    // flag concurrent-mutation hazards.
    unsafe {
        std::env::set_var(CONFIG_DIR_ENV, dir.path());
    }
    let result = f(&dir);
    // SAFETY: same lock.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
    }
    result
}

#[test]
fn load_returns_default_when_no_file_exists() {
    with_temp_dir(|_dir| {
        let value = persistence::load::<TestSketchSettings>();
        assert_eq!(value, TestSketchSettings::default());
    });
}

#[test]
fn save_then_load_round_trips() {
    with_temp_dir(|_dir| {
        let mut original = TestSketchSettings::default();
        original.widget_count = 123;
        original.tempo_hz = 1.25;
        original.enable_tint = false;
        original.tint_color = [0.1, 0.2, 0.3, 0.4];
        original.dev_label = String::from("custom");
        original.blend_mode = TestBlendMode::Multiply;

        persistence::save(&original);
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, original);
    });
}

#[test]
fn save_then_load_round_trips_empty_string_field() {
    with_temp_dir(|_dir| {
        // Mimics IMPORT: set the String field to a non-empty value, save, load.
        let mut original = TestSketchSettings::default();
        original.dev_label = String::from("blob/abc123.png");
        persistence::save(&original);
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(
            loaded.dev_label, "blob/abc123.png",
            "non-empty must persist"
        );

        // Mimics DELETE-CLEAR: clear the same field to "", save over the
        // existing file, load. Does an empty String round-trip through
        // toml::to_string_pretty + the load path, or does it get dropped?
        original.dev_label = String::new();
        persistence::save(&original);
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(
            loaded.dev_label, "",
            "empty string did not round-trip: {:?}",
            loaded.dev_label
        );
    });
}

#[test]
fn enum_field_absent_from_file_falls_back_to_default() {
    use std::fs;

    with_temp_dir(|_dir| {
        // A file written before `blend_mode` existed: section present, enum
        // field missing. `#[serde(default = ...)]` fills it in without
        // discarding the rest of the section.
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(
            &path,
            "[test]\nwidget_count = 7\ntempo_hz = 0.5\nenable_tint = true\n\
             tint_color = [1.0, 1.0, 1.0, 1.0]\ndev_label = \"kept\"\n",
        )
        .expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded.blend_mode, TestBlendMode::Normal);
        assert_eq!(loaded.widget_count, 7, "sibling fields must survive");
        assert_eq!(loaded.dev_label, "kept", "sibling fields must survive");
    });
}

#[test]
fn unknown_enum_variant_defaults_section_but_not_other_sections() {
    use std::fs;

    with_temp_dir(|_dir| {
        // An unknown variant string is a schema error for the `[test]`
        // section, so that one section falls back to defaults — the existing
        // per-section resilience (see `load_returns_default_when_section_
        // schema_mismatches`). Other sections in the same file are untouched.
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(
            &path,
            "[test]\nblend_mode = \"NotAVariant\"\n\n[unrelated]\nfoo = 42\n",
        )
        .expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, TestSketchSettings::default());

        // Saving after the failed load must not clobber the other section.
        persistence::save(&loaded);
        let text = fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("[unrelated]"),
            "[unrelated] section dropped: {text}"
        );
    });
}

#[test]
fn save_preserves_other_sections() {
    use std::fs;

    with_temp_dir(|_dir| {
        // Pre-seed a settings file with an unrelated section.
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(&path, "[unrelated]\nfoo = 42\nbar = \"keep me\"\n").expect("seed");

        // Save TestSketchSettings — should add a section, not clobber.
        let value = TestSketchSettings::default();
        persistence::save(&value);

        let text = fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("[unrelated]"),
            "[unrelated] section dropped: {text}"
        );
        assert!(text.contains("foo = 42"), "foo key dropped: {text}");
        assert!(
            text.contains(&format!("[{}]", TestSketchSettings::STORAGE_KEY)),
            "new section missing: {text}",
        );
    });
}

#[test]
fn load_returns_default_when_file_is_malformed_toml() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        fs::write(&path, "this is not valid toml = = =").expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, TestSketchSettings::default());
    });
}

#[test]
fn load_returns_default_when_section_schema_mismatches() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        // widget_count is u32 — feeding it a string triggers a schema error.
        fs::write(&path, "[test]\nwidget_count = \"not a number\"\n").expect("seed");

        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded, TestSketchSettings::default());
    });
}

/// Returns the sibling paths of the settings file whose name begins with
/// `sketch-settings.toml.corrupt-`. Used to assert a corrupt file was
/// quarantined rather than silently overwritten.
fn quarantine_files() -> Vec<std::path::PathBuf> {
    use std::fs;

    let path = persistence::settings_path();
    let dir = path.parent().expect("has parent").to_path_buf();
    let target = path.file_name().expect("has file name").to_os_string();
    let prefix = {
        let mut p = target;
        p.push(".corrupt-");
        p.to_string_lossy().into_owned()
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect()
}

#[test]
fn save_quarantines_corrupt_file_instead_of_clobbering() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");
        // A present-but-unparseable file. Under the old code this was silently
        // turned into an empty table and overwritten, destroying every other
        // section. The garbage must instead be quarantined.
        let garbage = "this is not valid toml = = = \x00 \u{fffd}";
        fs::write(&path, garbage).expect("seed");

        let mut settings = TestSketchSettings::default();
        settings.widget_count = 777;
        persistence::save(&settings);

        // (a) The corrupt file was preserved under a `.corrupt-*` name, with
        // its original bytes intact.
        let quarantined = quarantine_files();
        assert_eq!(
            quarantined.len(),
            1,
            "exactly one quarantine file expected, found: {quarantined:?}"
        );
        let recovered = fs::read_to_string(&quarantined[0]).expect("read quarantine");
        assert_eq!(
            recovered, garbage,
            "quarantine must hold the original bytes"
        );

        // The freshly-written file is valid and our section round-trips.
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded.widget_count, 777);
    });
}

#[test]
fn save_after_quarantine_preserves_sibling_sections() {
    use std::fs;

    with_temp_dir(|_dir| {
        let path = persistence::settings_path();
        fs::create_dir_all(path.parent().expect("has parent")).expect("mkdirs");

        // Corrupt file → first save quarantines it and writes a fresh `[test]`.
        fs::write(&path, "not = = valid toml").expect("seed");
        let mut first = TestSketchSettings::default();
        first.widget_count = 1;
        persistence::save(&first);
        assert_eq!(
            quarantine_files().len(),
            1,
            "corrupt file must be quarantined"
        );

        // A *different* sketch's section, saved after recovery. (Only one
        // settings type exists in the fixtures, so we inject the sibling by
        // hand; `save_preserves_other_sections` uses the same technique.)
        let mut text = fs::read_to_string(&path).expect("read after recovery");
        text.push_str("\n[unrelated]\nfoo = 42\nbar = \"keep me\"\n");
        fs::write(&path, text).expect("inject sibling section");

        // A subsequent save of `[test]` must NOT lose the sibling section.
        let mut second = TestSketchSettings::default();
        second.widget_count = 2;
        persistence::save(&second);

        let out = fs::read_to_string(&path).expect("read after second save");
        assert!(
            out.contains("[unrelated]"),
            "sibling section lost after subsequent save: {out}"
        );
        assert!(out.contains("foo = 42"), "sibling key lost: {out}");
        // And the section we just saved is current.
        let loaded = persistence::load::<TestSketchSettings>();
        assert_eq!(loaded.widget_count, 2);
    });
}
