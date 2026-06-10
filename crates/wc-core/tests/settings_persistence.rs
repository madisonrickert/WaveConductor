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
