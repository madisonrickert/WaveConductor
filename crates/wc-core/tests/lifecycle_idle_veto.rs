//! Integration tests for the idle-veto hook.

mod common;
use common::app::{arm_idle_timeline, lifecycle_test_app};

use bevy::prelude::*;
use wc_core::lifecycle::idle::RegisterIdleVetoExt;
use wc_core::lifecycle::state::{AppState, SketchActivity};

#[derive(Resource, Default)]
struct VetoFlag(bool);

fn flag_veto(world: &World) -> bool {
    world.get_resource::<VetoFlag>().is_some_and(|f| f.0)
}

#[test]
fn no_veto_means_idle_after_threshold() {
    let mut app = lifecycle_test_app();
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
    let mut app = lifecycle_test_app();
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
    let mut app = lifecycle_test_app();
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
