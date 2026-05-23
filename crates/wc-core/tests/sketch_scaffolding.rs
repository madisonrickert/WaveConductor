//! Sketch infrastructure tests using a dummy marker.

#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::lifecycle::state::{AppState, SketchActivity};
use wc_core::sketch::{despawn_with, sketch_active};

#[derive(Component)]
struct DummyRoot;

fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(StatesPlugin);
    app.init_state::<AppState>();
    app.add_sub_state::<SketchActivity>();
    app
}

#[test]
fn despawn_with_removes_tagged_entities() {
    let mut app = build_app();
    app.add_systems(Startup, |mut commands: Commands<'_, '_>| {
        commands.spawn(DummyRoot);
        commands.spawn(DummyRoot);
        commands.spawn(()); // untagged — should survive
    });
    app.add_systems(Update, despawn_with::<DummyRoot>);
    app.update(); // Startup spawns
    app.update(); // despawn_with runs and removes both DummyRoot entities
    let count = app
        .world_mut()
        .query::<&DummyRoot>()
        .iter(app.world())
        .count();
    assert_eq!(count, 0, "DummyRoot entities should be despawned");
}

#[derive(Resource, Default)]
struct Ran(bool);

fn mark_ran(mut r: ResMut<'_, Ran>) {
    r.0 = true;
}

#[test]
fn sketch_active_is_true_when_state_and_activity_match() {
    let mut app = build_app();

    // Install a resource and a system gated by sketch_active(AppState::Line).
    app.world_mut().insert_resource(Ran(false));
    app.add_systems(Update, mark_ran.run_if(sketch_active(AppState::Line)));

    // Default: AppState::Home, SketchActivity::Active.
    // System should not run because AppState != Line.
    app.update();
    assert!(
        !app.world().resource::<Ran>().0,
        "should not run when AppState != Line"
    );

    // Transition to Line.
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Line);
    app.update(); // state transition processed
    app.update(); // condition is now true and system runs
    assert!(
        app.world().resource::<Ran>().0,
        "should run when AppState == Line && SketchActivity == Active"
    );
}
