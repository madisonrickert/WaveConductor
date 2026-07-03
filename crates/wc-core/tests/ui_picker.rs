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
        settings_key: "line",
        screenshot: Handle::default(),
    });
    let manifest = app.world().resource::<SketchManifest>();
    assert!(
        manifest.get(AppState::Line).is_some(),
        "Line should be registered"
    );
    for state in [AppState::Dots, AppState::Cymatics, AppState::Waves] {
        assert!(
            manifest.get(state).is_none(),
            "{state:?} should be unregistered (placeholder)"
        );
    }
}

/// Iterating `SKETCH_ORDER` and partitioning by manifest presence yields
/// exactly 1 active entry and 3 placeholder entries when only `Line` is
/// registered.
///
/// `SKETCH_ORDER` has 4 entries (`Line`, `Flame`, `Dots`, `Cymatics`) —
/// `Waves` is a de-routed seam, not part of the cycle (`AUDIT.md` T5).
#[test]
fn sketch_order_iteration_yields_one_active_three_placeholder_when_only_line_registered() {
    let mut app = App::new();
    app.register_sketch_manifest(SketchManifestEntry {
        state: AppState::Line,
        display_name: "Line",
        settings_key: "line",
        screenshot: Handle::default(),
    });
    let manifest = app.world().resource::<SketchManifest>();
    let (active, placeholder): (Vec<&AppState>, Vec<&AppState>) = AppState::SKETCH_ORDER
        .iter()
        .partition(|s| manifest.get(**s).is_some());
    assert_eq!(active.len(), 1);
    assert_eq!(placeholder.len(), 3);
    assert_eq!(active[0], &AppState::Line);
}

/// Every `AppState::SKETCH_ORDER` entry must resolve to a *real, implemented*
/// sketch manifest — never a placeholder that is still reachable via
/// Next/Prev/number-key navigation (a `SKETCH_ORDER` entry with no manifest
/// is exactly the phantom black-screen state `Waves` used to be; `AUDIT.md`
/// T5).
///
/// `wc-core` cannot depend on `wc-sketches` (the dependency runs the other
/// way — `wc-sketches` depends on `wc-core`), so this test can't spin up the
/// real `LinePlugin`/`FlamePlugin`/`DotsPlugin`/`CymaticsPlugin` and check
/// their manifest registrations directly. Instead it pins the
/// known-implemented set below, mirroring the `register_sketch_manifest`
/// call each real sketch plugin makes today
/// (`crates/wc-sketches/src/{line,flame,dots,cymatics}/mod.rs`), and asserts
/// every `SKETCH_ORDER` entry is covered by it. If `SKETCH_ORDER` ever grows
/// a new entry, this test fails until `KNOWN_IMPLEMENTED_SKETCHES` is
/// updated too — a deliberate human/agent acknowledgement that the sketch is
/// really implemented, instead of the array silently re-admitting an
/// unimplemented placeholder the way `Flame`/`Waves` did.
#[test]
fn sketch_order_entries_are_all_known_implemented_sketches() {
    /// Mirrors the manifest entries each real sketch plugin registers.
    /// Update alongside `crates/wc-sketches/src/*/mod.rs` when a new sketch
    /// plugin starts (or stops) calling `register_sketch_manifest`.
    const KNOWN_IMPLEMENTED_SKETCHES: [AppState; 4] = [
        AppState::Line,
        AppState::Flame,
        AppState::Dots,
        AppState::Cymatics,
    ];

    let mut app = App::new();
    for state in KNOWN_IMPLEMENTED_SKETCHES {
        app.register_sketch_manifest(SketchManifestEntry {
            state,
            display_name: "test",
            settings_key: "test",
            screenshot: Handle::default(),
        });
    }
    let manifest = app.world().resource::<SketchManifest>();
    for state in AppState::SKETCH_ORDER {
        assert!(
            manifest.get(state).is_some(),
            "{state:?} is in SKETCH_ORDER but has no known-implemented sketch \
             manifest; either implement its plugin, register it, and add it \
             to KNOWN_IMPLEMENTED_SKETCHES above, or remove it from \
             SKETCH_ORDER"
        );
    }
}
