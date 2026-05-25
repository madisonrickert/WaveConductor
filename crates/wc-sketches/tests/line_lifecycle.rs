//! Line sketch lifecycle integration tests.
//!
//! Uses `MinimalPlugins` + just enough Bevy plugins to exercise the main-world
//! lifecycle (state transitions, entity spawn/despawn, settings registration)
//! without a GPU or render world. The render asset pipelines gracefully no-op
//! when `RenderApp` is absent.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use bevy::asset::AssetPlugin;
use bevy::prelude::*;
use bevy::render::storage::ShaderStorageBuffer;
use bevy::state::app::StatesPlugin;
use wc_core::input::pointer::PointerState;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::{settings::LineSettings, LinePlugin, LineRoot};

fn build_app() -> App {
    // Point config at a per-test temp dir so we don't stomp persisted settings.
    let dir = std::env::temp_dir().join(format!("wc-line-lifecycle-{}", std::process::id()));
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
    app.init_state::<AppState>();
    app.add_sub_state::<SketchActivity>();

    // Provide the action state + input map so `handle_dev_panel_toggle` in
    // `SettingsPlugin` doesn't panic on missing resource.
    app.add_plugins(leafwing_input_manager::plugin::InputManagerPlugin::<
        wc_core::lifecycle::actions::WaveConductorAction,
    >::default());
    app.insert_resource(wc_core::lifecycle::actions::default_input_map());
    app.init_resource::<leafwing_input_manager::prelude::ActionState<
        wc_core::lifecycle::actions::WaveConductorAction,
    >>();

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

#[test]
fn line_settings_resource_inserted() {
    let mut app = build_app();
    app.update();

    let settings = app
        .world()
        .get_resource::<LineSettings>()
        .expect("LineSettings should be inserted by LinePlugin");
    assert!(
        settings.particle_count >= 100,
        "particle_count should default to at least 100, got {}",
        settings.particle_count
    );
}

#[test]
fn enter_line_spawns_root_marker() {
    let mut app = build_app();
    app.update(); // initialize resources

    // Transition to AppState::Line.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update(); // state transition processed
    app.update(); // OnEnter system runs

    let count = app
        .world_mut()
        .query::<&LineRoot>()
        .iter(app.world())
        .count();
    assert!(
        count >= 1,
        "at least one LineRoot entity should exist after OnEnter(AppState::Line)"
    );
}

#[test]
fn exit_line_despawns_root_marker() {
    let mut app = build_app();
    app.update();

    // Enter Line.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Verify entities were spawned.
    let count_before = app
        .world_mut()
        .query::<&LineRoot>()
        .iter(app.world())
        .count();
    assert!(count_before >= 1, "LineRoot must exist before exit");

    // Exit Line.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Home);
    app.update();
    app.update();

    let count_after = app
        .world_mut()
        .query::<&LineRoot>()
        .iter(app.world())
        .count();
    assert_eq!(
        count_after, 0,
        "all LineRoot entities should be despawned after OnExit(AppState::Line)"
    );
}

#[test]
fn update_sim_params_does_not_run_when_idle() {
    use wc_sketches::line::compute::LineSimParams;

    let mut app = build_app();
    app.update();

    // Enter Line and let a couple frames run so LineSimParams is populated.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Record dt before going idle.
    let dt_before = app
        .world()
        .get_resource::<LineSimParams>()
        .map_or(0.0_f32, |p| p.params.dt);

    // Transition SketchActivity → Idle.
    app.world_mut()
        .resource_mut::<NextState<SketchActivity>>()
        .set(SketchActivity::Idle);
    app.update();

    // Advance time and confirm that update_sim_params did NOT run.
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(std::time::Duration::from_millis(100));
    app.update();

    let dt_after = app
        .world()
        .get_resource::<LineSimParams>()
        .map_or(0.0_f32, |p| p.params.dt);

    // Intentional bit-for-bit equality: if the system did not run, the value
    // must be exactly unchanged — not approximately equal.
    #[allow(
        clippy::float_cmp,
        reason = "bit-for-bit equality check: update_sim_params must not have written to sim.params.dt"
    )]
    {
        assert_eq!(
            dt_before, dt_after,
            "update_sim_params must not run while SketchActivity::Idle (dt changed)"
        );
    }
}

#[test]
fn settings_restart_cycles_back_to_line() {
    use wc_core::settings::SketchRestart;
    use wc_core::settings::SketchSettings;
    use wc_sketches::line::settings::LineSettings;

    let mut app = build_app();
    app.update();

    // Enter Line and let OnEnter run.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );

    // Emit a SketchRestart for LineSettings.
    app.world_mut().write_message(SketchRestart {
        storage_key: LineSettings::STORAGE_KEY,
    });
    // The trampoline takes multiple update cycles because Bevy applies state
    // transitions between schedules, not within a single Update. We don't try
    // to assert intermediate frames here — only that the cycle eventually
    // returns to Line. Five updates is more than enough headroom for both the
    // Home transition and the re-entry transition to land.
    for _ in 0..5 {
        app.update();
    }
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line,
        "settings restart should cycle Line → Home → Line within a few frames",
    );
}
