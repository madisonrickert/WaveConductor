//! `LineTemplateAdjustments` persists its hash-keyed map through the central
//! settings persistence (no separate file) — confirming the registered-resource
//! design serializes a `HashMap` field round-trip.

#![cfg(feature = "templates")]
#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; serialized via a static mutex"
)]
#![allow(
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test code; the [f32;2] values are exactly representable and round-trip"
)]

use std::sync::Mutex;

use wc_core::settings::persistence::{self, CONFIG_DIR_ENV};
use wc_sketches::line::template_adjustments::TemplateAdjustments;
use wc_sketches::line::template_adjustments_store::LineTemplateAdjustments;

fn with_temp_dir<R>(f: impl FnOnce() -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().expect("env mutex");
    let dir = std::env::temp_dir().join(format!("wc-line-adj-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir temp");
    let prev = std::env::var_os(CONFIG_DIR_ENV);
    // SAFETY: serialized by LOCK above.
    unsafe {
        std::env::set_var(CONFIG_DIR_ENV, &dir);
    }
    // Clean the settings file (`<temp dir>/waveconductor/sketch-settings.toml`)
    // *after* the env override lands, so it resolves inside the temp dir, and
    // via `settings_path()` rather than a hardcoded name so a rename of
    // `persistence::SETTINGS_FILE_NAME` cannot leave this quietly cleaning
    // nothing and letting a prior run leak state.
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
fn adjustments_map_round_trips() {
    with_temp_dir(|| {
        let mut s = LineTemplateAdjustments::default();
        s.map.insert(
            "deadbeef".into(),
            TemplateAdjustments {
                gamma: 2.0,
                color_influence: 0.5,
                invert: true,
                position: [0.25, -0.5],
                scale: [1.5, 0.75],
                ..Default::default()
            },
        );
        persistence::save(&s);

        let loaded = persistence::load::<LineTemplateAdjustments>();
        let got = loaded.map.get("deadbeef").expect("entry persisted");
        assert!((got.gamma - 2.0).abs() < 1e-6);
        assert!((got.color_influence - 0.5).abs() < 1e-6);
        assert!(got.invert);
        assert_eq!(got.position, [0.25, -0.5]);
        assert_eq!(got.scale, [1.5, 0.75]);
    });
}

#[test]
fn empty_map_round_trips() {
    with_temp_dir(|| {
        persistence::save(&LineTemplateAdjustments::default());
        let loaded = persistence::load::<LineTemplateAdjustments>();
        assert!(loaded.map.is_empty());
    });
}
