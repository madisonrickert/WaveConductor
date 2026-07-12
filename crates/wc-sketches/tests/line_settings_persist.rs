//! Confirms the real `LineSettings` struct round-trips an empty `spawn_template`
//! through the production `persistence::save` / `load` path (not the
//! `TestSketchSettings` proxy). Rules out a `LineSettings`-specific TOML
//! serialization quirk for the delete-clear ("") case.

#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; serialized via a static mutex"
)]
#![allow(clippy::expect_used, reason = "test code")]

use std::sync::Mutex;

use wc_core::settings::persistence::{self, CONFIG_DIR_ENV};
use wc_sketches::line::settings::LineSettings;

fn with_temp_dir<R>(f: impl FnOnce() -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().expect("env mutex");
    let dir = std::env::temp_dir().join(format!("wc-line-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    let prev = std::env::var_os(CONFIG_DIR_ENV);
    // SAFETY: serialized by LOCK above.
    unsafe {
        std::env::set_var(CONFIG_DIR_ENV, &dir);
    }
    // Start from a clean settings file (`<temp dir>/waveconductor/
    // sketch-settings.toml`) so a prior run can't leak state. Asked for *after*
    // the env override is in place, so it resolves inside the temp dir, and via
    // `settings_path()` rather than a hardcoded name so a rename of
    // `persistence::SETTINGS_FILE_NAME` cannot leave this silently cleaning
    // nothing.
    let _ = std::fs::remove_file(persistence::settings_path());
    let r = f();
    // SAFETY: same lock.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
    }
    r
}

#[test]
fn line_settings_round_trips_then_clears_spawn_template() {
    with_temp_dir(|| {
        // IMPORT: a managed-blob path is persisted.
        let mut s = LineSettings {
            spawn_template: String::from("/data/waveconductor/templates/deadbeef.png"),
            ..Default::default()
        };
        persistence::save(&s);
        let loaded = persistence::load::<LineSettings>();
        assert_eq!(
            loaded.spawn_template, "/data/waveconductor/templates/deadbeef.png",
            "non-empty spawn_template must persist (import)"
        );

        // DELETE-CLEAR: spawn_template cleared to "".
        s.spawn_template = String::new();
        persistence::save(&s);
        let loaded = persistence::load::<LineSettings>();
        assert_eq!(
            loaded.spawn_template, "",
            "cleared empty spawn_template must persist (delete), got {:?}",
            loaded.spawn_template
        );
    });
}
