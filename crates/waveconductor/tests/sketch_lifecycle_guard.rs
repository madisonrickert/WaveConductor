//! Guard: every navigable sketch wires an `OnEnter` lifecycle.
//!
//! Fix 1's bug class: a sketch registers a picker tile (so it is reachable from
//! the Home grid and via Next/Prev/number-key navigation) but its `OnEnter`
//! spawn systems get compiled out — a black screen you can still cycle into.
//! That bug was a *feature-forwarding* mismatch: the binary enabled
//! `body-tracking-mediapipe` on wc-core but not on wc-sketches, so Radiance's
//! whole body-gated lifecycle vanished from the shipped build while the tile
//! survived.
//!
//! This test lives in the **binary** crate on purpose: an integration test here
//! compiles wc-sketches with the binary's REAL default features (the same
//! `body-tracking-mediapipe` forward the shipped binary relies on), so a dropped
//! forward is caught here even though wc-sketches' own `--all-features` test
//! suite would still pass. It builds the full [`SketchesPlugin`] headlessly
//! (no `RenderApp`, no `update()`) and introspects the registered schedules:
//! every manifest-tiled `SKETCH_ORDER` entry must have a non-empty `OnEnter`
//! schedule, and Radiance specifically must still register `spawn_radiance`
//! (its unconditional camera-arbitration `OnEnter` system would survive a
//! compile-out and make the bare non-empty check pass, so it is pinned by name).

#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use bevy::asset::AssetPlugin;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use bevy::sprite_render::ColorMaterial;
use bevy::state::app::StatesPlugin;
use wc_core::lifecycle::state::AppState;
use wc_core::sketch::SketchManifest;
use wc_sketches::SketchesPlugin;

/// Build a headless app with the full `SketchesPlugin` and the main-world
/// prerequisites its constituent plugins need at *build* time (the render-world
/// halves all no-op without a `RenderApp`). Deliberately does NOT `update()` or
/// `finish()` — the test only introspects the schedules the plugins register.
fn sketches_app() -> App {
    // Point config at a per-process temp dir so SettingsPlugin persistence does
    // not touch a real profile.
    let dir = std::env::temp_dir().join(format!("wc-sketch-guard-{}", std::process::id()));
    // SAFETY: single-threaded test setup; Rust 1.80+ requires unsafe for env
    // mutation.
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
    app.add_plugins(wc_core::lifecycle::LifecyclePlugin);
    app.add_plugins(bevy::mesh::MeshPlugin);
    app.init_asset::<ShaderBuffer>();
    app.init_asset::<Image>();
    app.init_asset::<ColorMaterial>();
    // WindowPlugin registers these in production; the MinimalPlugins harness
    // must register the channels so resize/cursor MessageReaders validate.
    app.add_message::<bevy::window::WindowResized>();
    app.add_message::<bevy::window::CursorMoved>();
    // `Single<&Window>` params resolve against this synthetic window.
    app.world_mut().spawn(Window {
        resolution: (1280_u32, 720_u32).into(),
        ..Default::default()
    });
    app.add_plugins(wc_core::settings::SettingsPlugin);
    // Radiance's always-on edge-gizmo system needs GizmoConfigStore; production
    // gets it from DefaultPlugins. It never runs here (no update), but keep the
    // resource owner present in case a plugin build reads it.
    app.add_plugins(bevy::gizmos::GizmoPlugin);

    app.add_plugins(SketchesPlugin);
    app
}

/// Names of the systems registered into `OnEnter(state)`, via schedule
/// introspection. The schedule is initialized against the world (which only
/// registers system-param access — it does not require the params' resources to
/// exist) so `systems()` yields the real, named entries.
fn on_enter_system_names(app: &mut App, state: AppState) -> Vec<String> {
    let world = app.world_mut();
    // Take Schedules out of the world so `Schedule::initialize` can borrow the
    // world mutably without aliasing (and without tripping the `resource_scope`
    // reinsert guard); put it back afterward.
    let mut schedules = world
        .remove_resource::<Schedules>()
        .expect("Schedules resource present after plugin build");
    let names = {
        let schedule = schedules
            .get_mut(OnEnter(state))
            .expect("every manifest-tiled sketch must register an OnEnter schedule");
        schedule
            .initialize(world)
            .expect("OnEnter schedule initializes");
        schedule
            .systems()
            .expect("schedule was just initialized")
            .map(|(_, system)| system.name().to_string())
            .collect()
    };
    world.insert_resource(schedules);
    names
}

/// Every `SKETCH_ORDER` entry that renders an active picker tile must also
/// register a non-empty `OnEnter` lifecycle — no reachable tile may lead to a
/// black, lifeless state (Fix 1's bug class).
#[test]
fn every_tiled_sketch_registers_on_enter_systems() {
    let mut app = sketches_app();

    let tiled: Vec<AppState> = AppState::SKETCH_ORDER
        .into_iter()
        .filter(|state| {
            app.world()
                .resource::<SketchManifest>()
                .get(*state)
                .is_some()
        })
        .collect();
    assert!(
        !tiled.is_empty(),
        "SketchesPlugin registered no picker tiles at all — harness is broken"
    );

    for state in tiled {
        let names = on_enter_system_names(&mut app, state);
        assert!(
            !names.is_empty(),
            "{state:?} has a picker tile but its OnEnter schedule is empty — the \
             sketch is reachable via navigation yet spawns nothing (Fix 1's bug \
             class: a compiled-out or unwired sketch lifecycle behind a live tile)"
        );
    }
}

/// Fix 1 regression, pinned by name: Radiance's camera-arbitration `OnEnter`
/// system is unconditional and would survive a `body-tracking-mediapipe`
/// compile-out, so the bare non-empty check above would not catch the exact
/// bug. The body-gated spawn chain — `spawn_radiance` — must be present, which
/// it only is when the binary forwards the feature to wc-sketches.
#[test]
fn radiance_on_enter_includes_the_body_gated_spawn() {
    let mut app = sketches_app();
    assert!(
        app.world()
            .resource::<SketchManifest>()
            .get(AppState::Radiance)
            .is_some(),
        "Radiance must register a picker tile"
    );

    let names = on_enter_system_names(&mut app, AppState::Radiance);
    assert!(
        names.iter().any(|n| n.contains("spawn_radiance")),
        "OnEnter(Radiance) is missing spawn_radiance — the body-tracking-mediapipe \
         forward from the binary to wc-sketches is broken, so the sketch tile is \
         live but the aura never spawns (Fix 1). Registered systems: {names:?}"
    );
}
