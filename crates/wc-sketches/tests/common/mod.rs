//! Shared test fixtures for `wc-sketches` integration tests.
//!
//! Hosts the `sketches_test_app` builder (the `AssetPlugin` / `MeshPlugin` /
//! `LinePlugin` variant that needs a real `Window` entity) and an
//! `arm_idle_timeline` mirror that targets `wc_core::lifecycle::idle::
//! InteractionTimer`. Cargo's per-crate integration-test isolation prevents
//! cross-crate `tests/common/` sharing of code that depends on wc-core types,
//! so `arm_idle_timeline` is duplicated here rather than re-imported.
//!
//! Phase A of Plan 7.5 adds a `#[path = ...] pub mod input;` re-import of
//! `crates/wc-core/tests/common/input.rs` so the synthetic-event helpers
//! (the larger payload) are shared via path import rather than duplicated.

#![allow(
    dead_code,
    reason = "Test fixtures may be unused by some integration test binaries."
)]

use std::time::Duration;

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use wc_core::input::pointer::PointerState;
use wc_sketches::line::LinePlugin;

/// Build a sketches-test app: standard wc-core lifecycle harness plus
/// `AssetPlugin`, `MeshPlugin`, `ShaderStorageBuffer` registration,
/// `PointerState`, `SettingsPlugin`, `LinePlugin`, and a synthetic Window
/// entity at 1280x720 (so `Single<&Window>` system params resolve).
///
/// Sets `WAVECONDUCTOR_CONFIG_DIR` to a per-test temp dir so persisted
/// settings don't leak between tests.
pub fn sketches_test_app() -> App {
    // Point config at a per-test temp dir so we don't stomp persisted settings.
    let dir = std::env::temp_dir().join(format!("wc-sketches-test-{}", std::process::id()));
    // SAFETY: test-only mutation of env var. Rust 1.80+ requires unsafe.
    #[allow(
        unsafe_code,
        reason = "env mutation is safe in single-threaded test setup"
    )]
    unsafe {
        std::env::set_var("WAVECONDUCTOR_CONFIG_DIR", &dir);
    }

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(AssetPlugin::default());
    app.add_plugins(StatesPlugin);

    // LifecyclePlugin owns AppState / SketchActivity registration, the
    // InteractionTimer + IdleVetoes resources consulted by advance_activity,
    // the InputManagerPlugin for WaveConductorAction, the default input map,
    // and the ActionState resource. Including it here gives veto-aware tests
    // a realistic idle pipeline (advance_activity runs end-to-end) while
    // continuing to satisfy the other tests' resource expectations.
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // Register Mesh as an asset (MeshPlugin) and ShaderStorageBuffer
    // so spawn_line can call `meshes.add(...)` / `buffers.add(...)`.
    // The render-world uploads are no-ops without RenderApp.
    app.add_plugins(bevy::mesh::MeshPlugin);
    app.init_asset::<ShaderStorageBuffer>();

    // PointerState is normally registered by wc_core::input::InputPlugin.
    // Insert the default here so update_sim_params doesn't panic.
    app.init_resource::<PointerState>();

    // Single<&Window> needs an entity with a Window component. WindowPlugin
    // creates one in production; tests use MinimalPlugins, so spawn one
    // manually with a fixed resolution that matches the production default.
    app.world_mut().spawn(Window {
        resolution: (1280_u32, 720_u32).into(),
        ..Default::default()
    });

    // SettingsPlugin provides the settings registry + persistence.
    app.add_plugins(wc_core::settings::SettingsPlugin);

    // LinePlugin registers LineSettings, Material2dPlugin (gracefully no-ops
    // render setup without RenderApp), LineComputePlugin (same), and wires
    // OnEnter / OnExit systems.
    app.add_plugins(LinePlugin);

    app
}

/// Configure an app so its idle-transition tests can advance time
/// deterministically over a handful of update ticks. Mirror of
/// `wc-core::tests::common::app::arm_idle_timeline`, duplicated here
/// because Cargo's per-crate test isolation prevents cross-crate
/// `tests/common/` sharing of code that depends on wc-core types.
///
/// Installs `TimeUpdateStrategy::ManualDuration(80 ms)`, marks the interaction
/// timer at `Time::elapsed()` so `idle_for` starts at zero, and shrinks
/// `idle_threshold` to 50 ms (with `screensaver_threshold` bumped to 60 s
/// so accumulated ticks during the test don't overshoot into Screensaver).
///
/// **Required:** the app must already have `LifecyclePlugin` registered.
pub fn arm_idle_timeline(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        80,
    )));
    let now = app.world().resource::<Time>().elapsed();
    let mut timer = app
        .world_mut()
        .resource_mut::<wc_core::lifecycle::idle::InteractionTimer>();
    timer.mark(now);
    timer.idle_threshold = Duration::from_millis(50);
    timer.screensaver_threshold = Duration::from_secs(60);
}
