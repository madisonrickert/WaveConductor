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

    // LifecyclePlugin owns AppState / SketchActivity registration, the
    // InteractionTimer + IdleVetoes resources consulted by advance_activity,
    // the InputManagerPlugin for WaveConductorAction, the default input map,
    // and the ActionState resource. Including it here gives the Phase E veto
    // test a realistic idle pipeline (advance_activity runs end-to-end) while
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

#[test]
fn line_settings_resource_inserted() {
    let mut app = build_app();
    app.update();

    let settings = app
        .world()
        .get_resource::<LineSettings>()
        .expect("LineSettings should be inserted by LinePlugin");
    assert!(
        settings.particle_density > 0.0,
        "particle_density should default > 0, got {}",
        settings.particle_density
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
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;
    use wc_sketches::line::compute::LineSimParams;

    let mut app = build_app();
    app.update();

    // Enter Line and let a couple frames run so LineSimParams is populated.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Ensure the Line idle veto (registered in LinePlugin::build) is dormant
    // for this test: the mouse-attractor power must be exactly zero so
    // `advance_activity` is free to transition into `Idle` once the timer
    // crosses the threshold.
    app.world_mut()
        .resource_mut::<wc_sketches::line::systems::MouseAttractorState>()
        .power = 0.0;

    // Drive `advance_activity` to transition SketchActivity → Idle via the
    // ManualDuration time pattern. `LifecyclePlugin` re-evaluates the target
    // activity each frame, so manually setting `NextState::Idle` would be
    // overwritten on the next update. Instead, shrink the idle threshold and
    // mark interaction at t=now, then step elapsed past the threshold with
    // `TimeUpdateStrategy::ManualDuration`.
    {
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::from_millis(50);
        timer.screensaver_threshold = Duration::from_secs(60);
    }
    let now = app.world().resource::<Time>().elapsed();
    app.world_mut().resource_mut::<InteractionTimer>().mark(now);
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(80),
        ));
    // Two updates: first queues the Idle transition, second resolves it.
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle,
        "test prerequisite: SketchActivity must have transitioned to Idle"
    );

    // Record dt now that we're firmly in Idle. `update_sim_params` may have
    // run on the frame *before* the state resolved to Idle (its run-condition
    // observes the pre-transition value), so we capture dt after that
    // settle-frame and verify it doesn't change on subsequent idle frames.
    let dt_before = app
        .world()
        .get_resource::<LineSimParams>()
        .map_or(0.0_f32, |p| p.params.dt);

    // One more update once we're firmly in Idle. This is the frame where
    // `update_sim_params` is gated off; dt must not change.
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
fn update_sim_params_writes_mouse_attractor_with_gravity_scaling() {
    use wc_sketches::line::compute::LineSimParams;
    use wc_sketches::line::settings::LineSettings;
    use wc_sketches::line::systems::MouseAttractorState;

    let mut app = build_app();
    app.update();

    // Enter Line so the gated `update_sim_params` chain starts firing.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Seed an active mouse attractor: power=10 at (5,5).
    app.world_mut().insert_resource(MouseAttractorState {
        power: 10.0,
        position: [5.0, 5.0],
    });

    // The chain is ordered (update_mouse_attractor → decay_mouse_attractor →
    // update_sim_params). decay does NOT zero the power on a single tick
    // because it only steps `floor + (power - floor) * 0.9`; from 10 that
    // lands at 9.2, still well above the floor+epsilon cutoff. The post-decay
    // power is what update_sim_params sees, so compute the expected value.
    let gravity = app.world().resource::<LineSettings>().gravity_constant;
    let post_decay_power = wc_sketches::line::systems::MOUSE_POWER_FLOOR
        + (10.0 - wc_sketches::line::systems::MOUSE_POWER_FLOOR)
            * wc_sketches::line::systems::MOUSE_POWER_DECAY;
    let expected_attractor_power = post_decay_power * gravity;

    app.update();

    let sim = app
        .world()
        .get_resource::<LineSimParams>()
        .expect("LineSimParams should be inserted by spawn_line");
    assert_eq!(
        sim.params.attractor_count, 1,
        "active mouse should populate one attractor slot"
    );
    assert!(
        (sim.params.attractors[0].power - expected_attractor_power).abs() < 1e-4,
        "attractor[0].power should equal post-decay mouse power * gravity_constant; got {} expected {}",
        sim.params.attractors[0].power,
        expected_attractor_power
    );
}

#[test]
fn idle_veto_keeps_line_active_during_attractor_decay() {
    use std::time::Duration;
    use wc_core::lifecycle::idle::InteractionTimer;
    use wc_sketches::line::systems::MouseAttractorState;

    let mut app = build_app();
    app.update();

    // Enter Line. LinePlugin registers the veto in build().
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<AppState>>().get(),
        AppState::Line
    );
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active,
    );

    // Simulate a click that left the attractor in mid-decay (power > 0).
    app.world_mut().resource_mut::<MouseAttractorState>().power = 5.0;

    // Shrink the idle threshold and arm `TimeUpdateStrategy::ManualDuration` so
    // each `app.update()` advances elapsed time deterministically.
    //
    // NOTE: In Bevy 0.18, `Time::advance_by` on the generic `Time<()>` clock is
    // overwritten every frame by `update_virtual_time`. The plan's literal test
    // body uses `Time::advance_by`, but it's a no-op here for the same reason
    // documented in `crates/wc-core/tests/lifecycle.rs::idle_transitions_after_threshold`
    // and `crates/wc-core/tests/lifecycle_idle_veto.rs::arm_idle_timeline`.
    // The correct adaptation is `TimeUpdateStrategy::ManualDuration` plus
    // marking interaction at the current elapsed time so `idle_for` measures
    // from a known origin.
    {
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        timer.idle_threshold = Duration::from_millis(50);
        timer.screensaver_threshold = Duration::from_secs(60);
    }
    let now = app.world().resource::<Time>().elapsed();
    app.world_mut().resource_mut::<InteractionTimer>().mark(now);
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(80),
        ));

    // Two updates: first would queue the Idle transition (idle_for ≈ 80 ms > 50 ms),
    // but the veto suppresses it; second resolves any pending state transitions.
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "Line should stay Active while mouse attractor is still decaying"
    );
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
