//! Integration tests for the sketch picker's manifest-driven iteration.
//!
//! These tests exercise the [`SketchManifest`] registry contract without
//! requiring a render context. They verify that the picker's two code paths
//! (active tile vs. placeholder tile) are correctly driven by whether a
//! sketch has called `register_sketch_manifest`.

#![cfg(not(target_arch = "wasm32"))]

use bevy::prelude::*;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::{RegisterSketchManifestExt, SketchManifest, SketchManifestEntry};

/// Inserting a manifest entry for `Line` makes it reachable via `get`, while
/// all other sketch variants remain absent.
#[test]
fn manifest_distinguishes_registered_vs_unregistered_sketches() {
    let mut app = App::new();
    app.register_sketch_manifest(SketchManifestEntry {
        state: AppState::Line,
        display_name: "Line",
        screenshot: Handle::default(),
    });
    let manifest = app.world().resource::<SketchManifest>();
    assert!(
        manifest.get(AppState::Line).is_some(),
        "Line should be registered"
    );
    for state in [
        AppState::Flame,
        AppState::Dots,
        AppState::Cymatics,
        AppState::Waves,
    ] {
        assert!(
            manifest.get(state).is_none(),
            "{state:?} should be unregistered (placeholder)"
        );
    }
}

/// Iterating `SKETCH_ORDER` and partitioning by manifest presence yields
/// exactly 1 active entry and 4 placeholder entries when only `Line` is
/// registered.
#[test]
fn sketch_order_iteration_yields_one_active_four_placeholder_when_only_line_registered() {
    let mut app = App::new();
    app.register_sketch_manifest(SketchManifestEntry {
        state: AppState::Line,
        display_name: "Line",
        screenshot: Handle::default(),
    });
    let manifest = app.world().resource::<SketchManifest>();
    let (active, placeholder): (Vec<&AppState>, Vec<&AppState>) = AppState::SKETCH_ORDER
        .iter()
        .partition(|s| manifest.get(**s).is_some());
    assert_eq!(active.len(), 1);
    assert_eq!(placeholder.len(), 4);
    assert_eq!(active[0], &AppState::Line);
}
