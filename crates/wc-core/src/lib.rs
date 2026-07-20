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
// Visual-debugging scaffold: compiled out of release entirely (Option A hybrid
// gating). Both modules rely on `debug-assertions = false` in the release/soak
// profiles — see each module's docs and the guard comment on
// `[profile.release]` in the workspace `Cargo.toml`.
#[cfg(debug_assertions)]
pub mod capture;
#[cfg(debug_assertions)]
pub mod debug;
pub mod diagnostics;
pub mod frame_limiter;
pub mod input;
pub mod lifecycle;
pub mod platform;
pub mod render;
pub mod settings;
pub mod sketch;
// Long-run soak instrumentation: like `capture`, compiled out of release
// entirely (the module's own `//!` docs carry the detail).
#[cfg(debug_assertions)]
pub mod soak;
/// Image template library (native-only, behind the `templates` feature).
#[cfg(feature = "templates")]
pub mod templates;
pub mod ui;

use bevy::prelude::*;

/// Single plugin that bundles every wc-core subsystem.
///
/// Registered once by the binary crate.
pub struct CorePlugin;

impl Plugin for CorePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(lifecycle::LifecyclePlugin);
        app.add_plugins(input::HandTrackingPlugin);
        // Body tracking (BlazePose person detector + landmark/segmentation worker).
        // Inert until a sketch inserts BodyTrackingRequest (Radiance, Plan C).
        #[cfg(feature = "body-tracking-mediapipe")]
        app.add_plugins(input::body::BodyTrackingPlugin);
        // OBSBOT camera control: takes the camera's on-device AI/gesture
        // system out of the loop so it stops fighting the app's own tracking
        // (Windows-only device IO; documented no-op facade elsewhere).
        #[cfg(feature = "obsbot-camera-control")]
        app.add_plugins(input::obsbot::ObsbotControlPlugin);
        // Live camera preview in the settings dock (works with any webcam —
        // it taps the tracking workers' frames). Compiled whenever a
        // camera-consuming modality is, matching input::capture's gate.
        #[cfg(any(
            feature = "hand-tracking-mediapipe",
            feature = "body-tracking-mediapipe"
        ))]
        app.add_plugins(input::camera_preview::CameraPreviewPlugin);
        app.add_plugins(audio::AudioPlugin);
        app.add_plugins(settings::SettingsPlugin);
        // Frame-rate cap (restores GPU headroom; see the frame_limiter module
        // docs and docs/runbooks/dots-explode-gpu-saturation.md). Defaults to
        // 60 fps; change in the "Display" settings section or via
        // WAVECONDUCTOR_FPS_CAP (0 = uncapped).
        app.add_plugins(frame_limiter::FrameLimiterPlugin);
        // Visual-debugging scaffold — debug builds only (compiled out of
        // release). DebugPlugin inserts DebugToggles only when a WC_DEBUG_* var
        // is set; CapturePlugin wires the capture systems only when WC_CAPTURE
        // is set; SoakPlugin wires the soak instrumentation only when WC_SOAK is
        // set. A normal debug run with none of them carries essentially nothing.
        #[cfg(debug_assertions)]
        app.add_plugins(capture::CapturePlugin);
        #[cfg(debug_assertions)]
        app.add_plugins(debug::DebugPlugin);
        #[cfg(debug_assertions)]
        app.add_plugins(soak::SoakPlugin);
        app.add_plugins(ui::WaveConductorUiPlugin);
        #[cfg(feature = "templates")]
        app.add_plugins(templates::resource::TemplatesPlugin);
    }
}

/// Tests for [`CorePlugin`]'s composition.
///
/// **These deliberately never call `app.update()`.** `AudioPlugin`'s `Startup`
/// system (`audio::engine::start_audio_engine`) unconditionally spawns the
/// device-watcher OS thread, which builds a `cpal::Host` and enumerates output
/// devices every ~2 s. `add_plugins` alone never runs `Startup`, so no thread is
/// spawned here. The first test in this module that *does* call `update()` will
/// spawn one real cpal-enumerating thread **per test** in this binary — which is
/// tolerable (each `App`'s `DeviceWatcher` stops and joins its thread on drop) but
/// is not free, and is a surprise worth not tripping over.
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

    #[test]
    fn core_plugin_registers_ui_plugin() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(CorePlugin);
        // WaveConductorUiPlugin should at least be addable without panic.
        // Concrete behavior is tested in each sub-plugin's own tests.
        assert!(app.is_plugin_added::<crate::ui::WaveConductorUiPlugin>());
    }

    #[test]
    #[cfg(debug_assertions)]
    fn core_plugin_does_not_insert_debug_toggles_without_env() {
        // No WC_DEBUG_* set in the test process → DebugPlugin inserts nothing.
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_plugins(StatesPlugin);
        app.add_plugins(CorePlugin);
        assert!(app
            .world()
            .get_resource::<crate::debug::DebugToggles>()
            .is_none());
        // CaptureState is only meaningful with WC_CAPTURE; CorePlugin must still build.
    }
}
