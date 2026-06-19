//! `SettingsPlugin` assembly + `SketchRestart` event behavior.

#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; called once per process before any thread"
)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "expect/panic with a clear message are appropriate in test code"
)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::settings::{
    DevPanelVisible, RegisterSketchSettingsExt, SettingsPlugin, SettingsRegistry, SketchRestart,
};

mod common;
use common::TestSketchSettings;

fn make_app() -> App {
    // Isolate config dir so this test doesn't read the dev's real settings file.
    let dir = std::env::temp_dir().join(format!("wc-settings-plugin-test-{}", std::process::id()));
    // SAFETY: all invocations of make_app write the same idempotent value
    // (a stable per-process temp dir path derived from std::process::id()) to
    // the same env var, so repeated or concurrent writes converge on a
    // consistent result. If this binary is ever run with multiple threads,
    // guard this with a Mutex as settings_persistence.rs does.
    unsafe {
        std::env::set_var("WAVECONDUCTOR_CONFIG_DIR", &dir);
    }

    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::input::InputPlugin);
    app.add_plugins(StatesPlugin);
    app.add_plugins(leafwing_input_manager::plugin::InputManagerPlugin::<
        wc_core::lifecycle::actions::WaveConductorAction,
    >::default());
    // Insert the default input map so key presses can be translated to actions.
    app.insert_resource(wc_core::lifecycle::actions::default_input_map());
    app.init_resource::<leafwing_input_manager::prelude::ActionState<
        wc_core::lifecycle::actions::WaveConductorAction,
    >>();
    // EguiPlugin is intentionally omitted. Both panel systems guard with
    // `World::contains_resource::<EguiUserTextures>()` and return early
    // before constructing the `SystemState` that would build `EguiContexts`,
    // so no egui assets (and no wgpu context) are needed in this harness.
    app.add_plugins(SettingsPlugin);
    // TestSketchSettings is cfg(test) only; register it here so the tests
    // below have a concrete settings type to exercise.
    app.register_sketch_settings::<TestSketchSettings>();
    app
}

#[test]
fn plugin_registers_test_settings_resource_with_defaults() {
    let mut app = make_app();
    app.update();
    let value = app.world().resource::<TestSketchSettings>().clone();
    assert_eq!(value, TestSketchSettings::default());
}

#[test]
fn registry_lists_test_settings_after_plugin_init() {
    let mut app = make_app();
    app.update();
    let registry = app.world().resource::<SettingsRegistry>().clone();
    let keys: Vec<&str> = registry.entries.iter().map(|e| e.storage_key).collect();
    assert!(keys.contains(&"test"), "test storage key missing: {keys:?}");
}

#[test]
fn dev_panel_visible_resource_defaults_false() {
    let mut app = make_app();
    app.update();
    assert!(!app.world().resource::<DevPanelVisible>().0);
}

#[test]
fn mutating_requires_restart_field_emits_event() {
    let mut app = make_app();
    app.update(); // baseline snapshot

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .widget_count = 999;
    app.update(); // diff happens here

    let messages = app
        .world()
        .resource::<bevy::prelude::Messages<SketchRestart>>();
    let count = messages.iter_current_update_messages().count();
    assert!(count >= 1, "expected SketchRestart, got {count}");
    let key = messages
        .iter_current_update_messages()
        .next()
        .expect("at least one message")
        .storage_key;
    assert_eq!(key, "test");
}

#[test]
fn mutating_non_restart_field_does_not_emit_event() {
    let mut app = make_app();
    app.update();

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .tempo_hz = 2.0;
    app.update();

    let messages = app
        .world()
        .resource::<bevy::prelude::Messages<SketchRestart>>();
    let count = messages.iter_current_update_messages().count();
    assert_eq!(
        count, 0,
        "tempo_hz is not requires_restart but emitted {count} events"
    );
}

#[test]
fn second_register_with_different_type_lists_both() {
    use bevy::reflect::Reflect;
    use serde::{Deserialize, Serialize};
    use wc_core_macros::SketchSettings as DeriveSettings;

    #[derive(
        DeriveSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq,
    )]
    #[reflect(Resource, Default)]
    #[settings(storage_key = "second")]
    struct Second {
        #[setting(default = 1_u32, category = User)]
        n: u32,
    }

    let mut app = make_app();
    app.register_sketch_settings::<Second>();
    app.update();
    let registry = app.world().resource::<SettingsRegistry>().clone();
    let keys: Vec<&str> = registry.entries.iter().map(|e| e.storage_key).collect();
    assert!(keys.contains(&"test"));
    assert!(keys.contains(&"second"));
}

#[test]
fn toggling_dev_panel_via_action_updates_resource() {
    use leafwing_input_manager::prelude::Buttonlike as _;

    let mut app = make_app();
    app.update();
    assert!(!app.world().resource::<DevPanelVisible>().0);

    // Simulate Shift+D using leafwing's `Buttonlike::press(world)` which
    // sends `KeyboardInput` messages that `keyboard_input_system` processes
    // in `PreUpdate`, letting leafwing translate them to `ToggleDevPanel`
    // before `handle_dev_panel_toggle` runs in `Update`.
    bevy::prelude::KeyCode::ShiftLeft.press(app.world_mut());
    bevy::prelude::KeyCode::KeyD.press(app.world_mut());
    app.update();
    assert!(
        app.world().resource::<DevPanelVisible>().0,
        "Shift+D should make DevPanelVisible true"
    );
}

#[test]
fn full_app_schedule_runs_without_panicking() {
    // Smoke test: 30 frames of updates must not panic with egui absent.
    // The panel systems guard with `World::contains_resource::<EguiUserTextures>()`
    // and return early before constructing the `SystemState` that would build
    // `EguiContexts` — so the 30-frame loop runs without ever touching an egui
    // context, and never panics from a missing one.
    let mut app = make_app();
    for _ in 0..30 {
        app.update();
    }
}

#[test]
fn autosave_fires_after_debounce_window() {
    use std::time::Duration;

    let mut app = make_app();
    app.update(); // baseline

    // Mutate
    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .tempo_hz = 2.5;

    // In Bevy 0.18, `Time<()>` is overwritten each frame by `update_virtual_time`
    // which derives it from `Time<Virtual>` and `Time<Real>`. Direct
    // `Time::advance_by` is therefore NOT the right way to control elapsed time
    // in tests. Use `TimeUpdateStrategy::ManualDuration` so each `app.update()`
    // advances `Time<()>.delta_secs()` by the given amount.
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));

    // Advance time past the debounce window in 100 ms chunks.
    // DEBOUNCE_SECS = 0.5, so 7 steps of 100 ms (700 ms total) is ample.
    for _ in 0..7_u32 {
        app.update();
    }

    // Reload from disk and confirm the value persisted.
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert!(
        (loaded.tempo_hz - 2.5).abs() < 1e-6,
        "got {}",
        loaded.tempo_hz
    );
}

/// Mutate the `String` field `field_name` of `TestSketchSettings` through the
/// *exact* reflection path the settings panel uses: clone the `AppTypeRegistry`
/// Arc, fetch `ReflectResource`, take a `Mut<dyn Reflect>` over the resource,
/// descend into the struct, `try_downcast_mut::<String>()`, and apply `set`.
/// The outer `Mut` deref is what arms Bevy change detection — identical to
/// `render_section_by_key` → `render_template_library`.
fn mutate_string_via_reflect(app: &mut App, field_name: &str, set: impl FnOnce(&mut String)) {
    use bevy::ecs::reflect::ReflectResource;
    use bevy::reflect::ReflectMut;

    let registry = app.world().resource::<AppTypeRegistry>().clone();
    let reflect_resource = registry
        .read()
        .get_type_data::<ReflectResource>(std::any::TypeId::of::<TestSketchSettings>())
        .cloned()
        .expect("ReflectResource registered for TestSketchSettings");
    let mut reflect_mut = reflect_resource
        .reflect_mut(app.world_mut())
        .expect("TestSketchSettings resource present");
    let reflect: &mut dyn bevy::reflect::Reflect = &mut *reflect_mut;
    match reflect.reflect_mut() {
        ReflectMut::Struct(s) => {
            let field = s.field_mut(field_name).expect("field exists");
            let v = field
                .try_downcast_mut::<String>()
                .expect("field is a String");
            set(v);
        }
        _ => panic!("TestSketchSettings is not a struct"),
    }
}

/// Hypothesis (b), via the production `reflect_mut` path: set a `String` setting
/// to a non-empty value (mimics template IMPORT), confirm it persists, then
/// CLEAR it to "" (mimics template DELETE) and confirm the empty value also
/// reaches disk after the debounce window.
#[test]
fn autosave_persists_empty_string_via_reflect_mut() {
    use std::time::Duration;

    let mut app = make_app();
    app.update(); // baseline snapshot
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));

    // --- Phase 1: IMPORT (set non-empty) ---
    mutate_string_via_reflect(&mut app, "dev_label", |s| *s = String::from("blob/abc.png"));
    for _ in 0..7_u32 {
        app.update();
    }
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(
        loaded.dev_label, "blob/abc.png",
        "non-empty value set via reflect_mut must persist"
    );

    // --- Phase 2: DELETE (clear to "") ---
    mutate_string_via_reflect(&mut app, "dev_label", String::clear);
    for _ in 0..7_u32 {
        app.update();
    }
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(
        loaded.dev_label, "",
        "cleared empty value set via reflect_mut must persist, got {:?}",
        loaded.dev_label
    );
}

/// Same hypothesis (b) using plain `resource_mut` (isolates persistence from the
/// reflect layer). If this passes but the reflect variant fails, the bug is in
/// the reflect path; if both pass, the empty value persists in isolation and the
/// real cause is elsewhere (timing / reload).
#[test]
fn autosave_persists_empty_string_via_resource_mut() {
    use std::time::Duration;

    let mut app = make_app();
    app.update();
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .dev_label = String::from("blob/abc.png");
    for _ in 0..7_u32 {
        app.update();
    }
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(loaded.dev_label, "blob/abc.png");

    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .dev_label
        .clear();
    for _ in 0..7_u32 {
        app.update();
    }
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(
        loaded.dev_label, "",
        "cleared empty value must persist via resource_mut, got {:?}",
        loaded.dev_label
    );
}

/// Replicates the real panel timing: the settings dock marks the resource
/// changed EVERY frame (the `reflect_mut` deref), which continuously resets the
/// debounce timer so `tick` never fires while the dock is open. The only save is
/// then `flush_on_exit`. Confirms the empty value still reaches disk under this
/// continuous-re-arm pattern (i.e. timing is not the asymmetry).
#[test]
fn continuous_rearm_then_exit_persists_empty_string() {
    use std::time::Duration;

    let mut app = make_app();
    app.update();
    app.world_mut()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            Duration::from_millis(100),
        ));

    // Dock "open" with a non-empty value: touch every frame (no value change),
    // re-arming the debounce so the tick never fires.
    mutate_string_via_reflect(&mut app, "dev_label", |s| *s = String::from("blob/x.png"));
    for _ in 0..10_u32 {
        mutate_string_via_reflect(&mut app, "dev_label", |_| {});
        app.update();
    }
    // Clear (delete), then keep the dock "open" (re-arming) for a while.
    mutate_string_via_reflect(&mut app, "dev_label", String::clear);
    for _ in 0..10_u32 {
        mutate_string_via_reflect(&mut app, "dev_label", |_| {});
        app.update();
    }
    // Quit: flush_on_exit is the only save opportunity.
    app.world_mut().write_message(bevy::app::AppExit::Success);
    app.update();

    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(
        loaded.dev_label, "",
        "continuous re-arm + exit must still persist empty, got {:?}",
        loaded.dev_label
    );
}

/// Hypothesis (b), the `flush_on_exit` path: set then clear a String field and
/// let an `AppExit`-triggered flush (not the debounce tick) write it. Confirms
/// the empty value is not lost on shutdown specifically.
#[test]
fn flush_on_exit_persists_empty_string() {
    let mut app = make_app();
    app.update();

    // Set non-empty, arm the debounce (no time advance so `tick` won't fire),
    // then exit-flush.
    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .dev_label = String::from("blob/x.png");
    app.update(); // detect_changes arms the pending timer
    app.world_mut().write_message(bevy::app::AppExit::Success);
    app.update(); // flush_on_exit drains pending + saves
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(loaded.dev_label, "blob/x.png");

    // Clear and exit-flush again.
    app.world_mut()
        .resource_mut::<TestSketchSettings>()
        .dev_label
        .clear();
    app.update();
    app.world_mut().write_message(bevy::app::AppExit::Success);
    app.update();
    let loaded = wc_core::settings::persistence::load::<TestSketchSettings>();
    assert_eq!(
        loaded.dev_label, "",
        "flush_on_exit must persist empty, got {:?}",
        loaded.dev_label
    );
}
