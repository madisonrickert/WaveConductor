//! Radiance sketch lifecycle integration tests.
//!
//! Mirrors `line_lifecycle.rs`: `MinimalPlugins` + just enough Bevy plugins
//! to exercise `RadiancePlugin`'s main-world lifecycle (state transitions,
//! entity spawn/despawn, activation-request insert/remove, activity sync)
//! without a GPU or render world. This is also the one place that actually
//! builds and updates the full `App` with `RadiancePlugin` wired in — the
//! per-system unit tests in `systems/spawn.rs` / `systems/activity.rs` call
//! each system directly and cannot catch an ambiguous `SystemTypeSet` from a
//! system registered twice (the AGENTS.md house hazard: it panics at
//! startup, and CI does not catch it). Feature-gated: the whole surface
//! under test lives behind `body-tracking-mediapipe`.
//!
//! `RadiancePlugin::build` also registers four always-on sanctioned
//! listeners (`restart_on_settings_change`, `reload_on_resize_settled`, the
//! window-resize debounce, `resume_hand_camera_when_due`) and the material
//! driver with no `sketch_active`/Idle-excluding `run_if` — i.e. "keeps
//! running through Idle/Screensaver" is a registration-shape property (see
//! `radiance::mod` `build()`), verified there by code inspection, the same way
//! flame's identical `drive_flame_material` gating has no dedicated runtime
//! test either. This file covers the dynamic behaviors instead: entry
//! spawns + requests, exit despawns + removes requests, and Idle pausing the
//! requests + freezing emission.

#![cfg(feature = "body-tracking-mediapipe")]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

mod common;
use common::arm_idle_timeline;

use bevy::asset::AssetPlugin;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bevy::sprite_render::Material2dPlugin;
use bevy::state::app::StatesPlugin;
use wc_core::audio::input::AudioCaptureRequest;
use wc_core::input::body::BodyTrackingRequest;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::radiance::compute::pipeline::RadianceComputePlugin;
use wc_sketches::radiance::render::{RadianceMaterial, RadianceSilhouetteMaterial};
use wc_sketches::radiance::systems::spawn::RadianceRoot;
use wc_sketches::radiance::RadiancePlugin;

/// Build a Radiance-only test app: standard wc-core lifecycle harness plus
/// the asset/material/compute plugins `RadiancePlugin` needs and a synthetic
/// 1280x720 `Window` entity (`Single<&Window>` system params resolve).
///
/// Deliberately does not add `wc_core::input::provider::ProviderRegistry` —
/// arbitration's `Option<ResMut<ProviderRegistry>>` params degrade to the
/// documented no-op branch, exactly like the headless case
/// `arbitration.rs`'s own `missing_registry_is_a_no_op` test covers.
fn radiance_test_app() -> App {
    let dir =
        std::env::temp_dir().join(format!("wc-sketches-radiance-test-{}", std::process::id()));
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

    // LifecyclePlugin owns AppState / SketchActivity, InteractionTimer +
    // IdleVetoes, and the input plumbing `advance_activity` and the
    // navigation systems need (see `common::sketches_test_app`'s identical
    // rationale).
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);

    // Mesh + ShaderBuffer + Image assets: spawn_radiance/ensure_body_surfaces
    // allocate all three. The render-world uploads are no-ops without
    // RenderApp.
    app.add_plugins(bevy::mesh::MeshPlugin);
    app.init_asset::<ShaderBuffer>();
    app.init_asset::<Image>();

    // WindowResized: WindowPlugin registers this in production; the
    // MinimalPlugins harness has to register the message channel explicitly
    // (mirrors `common::sketches_test_app`'s identical comment) so the
    // window-resize debounce listener's MessageReader doesn't fail
    // validation.
    app.add_message::<bevy::window::WindowResized>();

    // Single<&Window> needs an entity with a Window component.
    app.world_mut().spawn(Window {
        resolution: (1280_u32, 720_u32).into(),
        ..Default::default()
    });

    // SettingsPlugin provides the settings registry + persistence.
    app.add_plugins(wc_core::settings::SettingsPlugin);

    // Production render-material + compute wiring (SketchesPlugin registers
    // these once for the same reasons documented there); both gracefully
    // no-op without RenderApp.
    app.add_plugins(Material2dPlugin::<RadianceMaterial>::default());
    app.add_plugins(Material2dPlugin::<RadianceSilhouetteMaterial>::default());
    app.add_plugins(RadianceComputePlugin);

    // `systems::debug::draw_edge_debug` (Task 13) always registers (it
    // early-outs on the `edge_debug` Dev bool internally, not via a
    // feature/run_if gate) and its `Gizmos` system param needs
    // `GizmoConfigStore`, which production gets from `DefaultPlugins`
    // (bevy's `bevy_gizmos_render` feature). Headless harnesses don't add
    // `DefaultPlugins`, so add the resource-owning half explicitly.
    app.add_plugins(bevy::gizmos::GizmoPlugin);

    app.add_plugins(RadiancePlugin);

    app
}

/// Drive the app into `AppState::Radiance`. Three updates: the first two
/// settle the state transition (mirrors `line_lifecycle.rs`'s
/// `enter_line_spawns_root_marker`), the third gives the chained
/// `OnEnter(AppState::Radiance)` systems (arbitration → surfaces → spawn →
/// requests) a settled frame to run against.
fn enter_radiance(app: &mut App) {
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Radiance);
    app.update();
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Radiance,
        "test prerequisite: state must have entered Radiance"
    );
}

#[test]
fn enter_radiance_spawns_root_and_requests() {
    let mut app = radiance_test_app();
    app.update(); // initialize resources

    enter_radiance(&mut app);

    let root_count = app
        .world_mut()
        .query::<&RadianceRoot>()
        .iter(app.world())
        .count();
    assert_eq!(
        root_count, 2,
        "silhouette quad + billboard mesh should both be spawned under RadianceRoot"
    );
    assert!(
        app.world().get_resource::<AudioCaptureRequest>().is_some(),
        "entry should insert AudioCaptureRequest"
    );
    assert!(
        app.world().get_resource::<BodyTrackingRequest>().is_some(),
        "entry should insert BodyTrackingRequest"
    );
}

#[test]
fn exit_radiance_despawns_and_removes_requests() {
    let mut app = radiance_test_app();
    app.update();

    enter_radiance(&mut app);
    assert!(
        app.world().get_resource::<AudioCaptureRequest>().is_some(),
        "test prerequisite: requests must exist before exit"
    );

    // Exit back to Home.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Home);
    app.update();
    app.update();

    let root_count = app
        .world_mut()
        .query::<&RadianceRoot>()
        .iter(app.world())
        .count();
    assert_eq!(
        root_count, 0,
        "all RadianceRoot entities should be despawned after OnExit(AppState::Radiance)"
    );
    assert!(
        app.world().get_resource::<AudioCaptureRequest>().is_none(),
        "exit should remove AudioCaptureRequest (stopping the mic stream)"
    );
    assert!(
        app.world().get_resource::<BodyTrackingRequest>().is_none(),
        "exit should remove BodyTrackingRequest (stopping the body worker)"
    );
}

#[test]
fn idle_pauses_tracking_requests_and_freezes_emission() {
    use wc_sketches::radiance::compute::sim_params::RadianceSimParams;

    let mut app = radiance_test_app();
    app.update();

    enter_radiance(&mut app);
    assert!(
        !app.world().resource::<AudioCaptureRequest>().paused,
        "test prerequisite: capture starts unpaused"
    );
    assert!(
        !app.world().resource::<BodyTrackingRequest>().idle_throttle,
        "test prerequisite: tracking starts unthrottled"
    );

    // Drive SketchActivity -> Idle via the shared arm_idle_timeline helper
    // (shrinks idle_threshold, marks interaction at `now`, installs
    // TimeUpdateStrategy::ManualDuration). Two updates: first queues the
    // Idle transition, second resolves it (mirrors
    // `line_lifecycle.rs::update_sim_params_does_not_run_when_idle`).
    arm_idle_timeline(&mut app);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle,
        "test prerequisite: SketchActivity must have transitioned to Idle"
    );

    assert!(
        app.world().resource::<AudioCaptureRequest>().paused,
        "OnEnter(Idle) should pause the mic capture"
    );
    assert!(
        app.world().resource::<BodyTrackingRequest>().idle_throttle,
        "OnEnter(Idle) should throttle body tracking"
    );

    let sim = app.world().resource::<RadianceSimParams>();
    assert!(
        sim.params.emission_prob.abs() < f32::EPSILON,
        "OnEnter(Idle) should freeze emission to 0"
    );
    assert!(
        sim.params.burst_speed.abs() < f32::EPSILON,
        "OnEnter(Idle) should freeze the burst speed to 0"
    );
}
