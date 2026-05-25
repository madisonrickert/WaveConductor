//! Integration tests for the idle-veto hook.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use std::time::Duration;

use bevy::prelude::*;
use wc_core::lifecycle::idle::{InteractionTimer, RegisterIdleVetoExt};
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::lifecycle::LifecyclePlugin;

#[derive(Resource, Default)]
struct VetoFlag(bool);

fn flag_veto(world: &World) -> bool {
    world.get_resource::<VetoFlag>().is_some_and(|f| f.0)
}

/// Build a headless test app mirroring `crates/wc-core/tests/lifecycle.rs`. The
/// `LifecyclePlugin` itself registers `InputManagerPlugin`, the input map, the
/// `ActionState`, and (in Phase A) the `IdleVetoes` resource — so this helper
/// just adds the supporting Bevy plugins.
fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    // StatesPlugin is required for the StateTransition schedule used by
    // init_state / add_sub_state inside LifecyclePlugin.
    app.add_plugins(bevy::state::app::StatesPlugin);
    // InputPlugin is needed for ButtonInput resources used by leafwing.
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(LifecyclePlugin);
    app
}

/// Shrink the idle threshold so tests don't have to march for 30 s of virtual
/// time, then mark interaction at t=0 and switch to `ManualDuration` so each
/// subsequent `update()` advances elapsed by the configured step. Mirrors the
/// pattern documented in `tests/lifecycle.rs::idle_transitions_after_threshold`
/// (Bevy 0.18 quirk: `Time::advance_by` is overwritten each frame by
/// `update_virtual_time`).
fn arm_idle_timeline(app: &mut App) {
    {
        let mut timer = app.world_mut().resource_mut::<InteractionTimer>();
        // Idle fires after one 80 ms manual tick. Screensaver threshold is set
        // far enough out that the veto-clearing test, which accumulates several
        // ticks while the veto is held, lands in `Idle` rather than
        // overshooting straight into `Screensaver`.
        timer.idle_threshold = Duration::from_millis(50);
        timer.screensaver_threshold = Duration::from_secs(60);
    }
    let now = app.world().resource::<Time>().elapsed();
    app.world_mut().resource_mut::<InteractionTimer>().mark(now);
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(80),
        ));
}

#[test]
fn no_veto_means_idle_after_threshold() {
    let mut app = build_app();
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update(); // Resolve transition: AppState::Line, SketchActivity::Active.

    arm_idle_timeline(&mut app);

    // Two updates: first queues the Idle transition (idle_for ≈ 80 ms > 50 ms
    // threshold), second resolves it.
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Idle,
        "no veto registered → idle transition fires"
    );
}

#[test]
fn active_veto_keeps_sketch_active() {
    let mut app = build_app();
    app.init_resource::<VetoFlag>();
    app.register_idle_veto(flag_veto);
    app.world_mut().resource_mut::<VetoFlag>().0 = true;

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();

    arm_idle_timeline(&mut app);
    app.update();
    app.update();

    let activity = app.world().resource::<State<SketchActivity>>();
    assert_eq!(
        *activity.get(),
        SketchActivity::Active,
        "veto override → sketch stays Active despite elapsed idle time"
    );
}

#[test]
fn veto_clearing_lets_sketch_idle() {
    let mut app = build_app();
    app.init_resource::<VetoFlag>();
    app.register_idle_veto(flag_veto);

    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update();

    // Veto active → stays Active across idle threshold.
    app.world_mut().resource_mut::<VetoFlag>().0 = true;
    arm_idle_timeline(&mut app);
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Active
    );

    // Veto cleared → next frame transitions to Idle.
    app.world_mut().resource_mut::<VetoFlag>().0 = false;
    app.update();
    app.update();
    assert_eq!(
        *app.world().resource::<State<SketchActivity>>().get(),
        SketchActivity::Idle
    );
}
