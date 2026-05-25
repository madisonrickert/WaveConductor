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

mod common;
use common::{arm_idle_timeline, sketches_test_app};

use bevy::prelude::*;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_sketches::line::systems::{MOUSE_POWER_DECAY, MOUSE_POWER_FLOOR};
use wc_sketches::line::{settings::LineSettings, LineRoot};

/// Initial seeded power for the gravity-scaling lifecycle test.
const SEEDED_MOUSE_POWER: f32 = 10.0;

/// Expected post-decay power after one tick of `decay_mouse_attractor`:
/// `floor + (power - floor) * decay`. Derived from the same v4 constants the
/// production decay system uses, so the test follows tuning changes
/// automatically.
const EXPECTED_POST_DECAY_POWER: f32 =
    MOUSE_POWER_FLOOR + (SEEDED_MOUSE_POWER - MOUSE_POWER_FLOOR) * MOUSE_POWER_DECAY;

#[test]
fn line_settings_resource_inserted() {
    let mut app = sketches_test_app();
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
    let mut app = sketches_test_app();
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
    let mut app = sketches_test_app();
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

    let mut app = sketches_test_app();
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
    // shared `arm_idle_timeline` helper (shrinks idle threshold, marks
    // interaction at `now`, installs `TimeUpdateStrategy::ManualDuration`).
    // `LifecyclePlugin` re-evaluates the target activity each frame, so
    // manually setting `NextState::Idle` would be overwritten on the next
    // update — stepping elapsed past the threshold is the correct path.
    arm_idle_timeline(&mut app);
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
fn idle_veto_keeps_line_active_during_attractor_decay() {
    use wc_sketches::line::systems::MouseAttractorState;

    let mut app = sketches_test_app();
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
    // each `app.update()` advances elapsed time deterministically. See
    // `arm_idle_timeline` for the Bevy 0.18 quirk this works around
    // (`Time::advance_by` is overwritten by `update_virtual_time`).
    arm_idle_timeline(&mut app);

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
fn update_sim_params_writes_mouse_attractor_with_gravity_scaling() {
    use wc_sketches::line::compute::LineSimParams;
    use wc_sketches::line::settings::LineSettings;
    use wc_sketches::line::systems::MouseAttractorState;

    let mut app = sketches_test_app();
    app.update();

    // Enter Line so the gated `update_sim_params` chain starts firing.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();
    app.update();

    // Seed an active mouse attractor at (5, 5) with the test-fixed starting
    // power. `EXPECTED_POST_DECAY_POWER` (module scope) computes the post-tick
    // value from the same v4 constants the production code uses.
    app.world_mut().insert_resource(MouseAttractorState {
        power: SEEDED_MOUSE_POWER,
        position: [5.0, 5.0],
    });

    // The chain is ordered (update_mouse_attractor → decay_mouse_attractor →
    // update_sim_params). decay does NOT zero the power on a single tick
    // because it only steps `floor + (power - floor) * decay`; from 10 that
    // lands well above the floor+epsilon cutoff. The post-decay power is what
    // update_sim_params sees.
    let gravity = app.world().resource::<LineSettings>().gravity_constant;
    let expected_attractor_power = EXPECTED_POST_DECAY_POWER * gravity;

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
fn settings_restart_cycles_back_to_line() {
    use wc_core::settings::SketchRestart;
    use wc_core::settings::SketchSettings;
    use wc_sketches::line::settings::LineSettings;

    let mut app = sketches_test_app();
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
