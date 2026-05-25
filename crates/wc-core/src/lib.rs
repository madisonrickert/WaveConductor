//! # wc-core
//!
//! Shared infrastructure for `WaveConductor` v5: lifecycle, input, audio,
//! settings, and math helpers. Sketches consume this crate via [`CorePlugin`];
//! the binary crate registers `CorePlugin` once at app startup.

// Allow `::wc_core::...` paths to resolve inside this crate itself, which
// the `#[derive(SketchSettings)]` macro emits for all trait implementations.
// Unused inside the lib since `test_settings` moved to `tests/common/`, but
// retained so any future in-crate use of the derive (Plan 8+ sketches owned
// by wc-core) continues to compile without re-introducing the extern crate.
#[allow(
    unused_extern_crates,
    reason = "kept for future in-crate macro consumers"
)]
extern crate self as wc_core;

pub mod audio;
pub mod input;
pub mod lifecycle;
pub mod settings;
pub mod sketch;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
        app.add_plugins(input::HandTrackingPlugin);
        app.add_plugins(audio::AudioPlugin);
        app.add_plugins(settings::SettingsPlugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;

    #[test]
    fn core_plugin_builds_without_panicking() {
        // NOTE: `EguiPlugin` is intentionally omitted — it requires `Assets<Shader>`
        // which is only present with `DefaultPlugins` (not `MinimalPlugins`).
        // Phase A panel stubs don't add any egui systems, so the plugin compiles
        // cleanly without it. Phase B will require a richer test harness.
        //
        // `CorePlugin` → `LifecyclePlugin` adds `InputManagerPlugin` and
        // `ActionState`, so we must NOT add them again here.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(CorePlugin);
    }
}
