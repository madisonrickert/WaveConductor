//! `SettingsPlugin` assembly + `SketchRestart` event behavior.

#![allow(
    unsafe_code,
    reason = "Rust 1.80+ marks env::set_var unsafe; called once per process before any thread"
)]
#![allow(
    clippy::expect_used,
    reason = "expect with a clear message is appropriate in test code"
)]

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use wc_core::settings::{
    test_settings::TestSketchSettings, DevPanelVisible, RegisterSketchSettingsExt, SettingsPlugin,
    SettingsRegistry, SketchRestart,
};

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
    app.init_resource::<leafwing_input_manager::prelude::ActionState<
        wc_core::lifecycle::actions::WaveConductorAction,
    >>();
    // EguiPlugin requires `Assets<Shader>` from DefaultPlugins; Phase A panel stubs
    // don't add egui systems, so EguiPlugin is not needed for Phase A tests.
    // Phase B will require a richer harness (e.g. wgpu headless or mock contexts).
    app.add_plugins(SettingsPlugin);
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
