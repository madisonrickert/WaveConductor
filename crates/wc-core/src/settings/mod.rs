//! Settings subsystem: typed per-sketch settings, persistence, restart
//! events, user panel, and dev inspector.
//!
//! ## Data flow
//!
//! 1. A sketch crate defines `MySettings` with `#[derive(SketchSettings)]`.
//!    The macro emits `Default` and the [`SketchSettings`] impl.
//! 2. The sketch's plugin calls
//!    `app.register_sketch_settings::<MySettings>()`. This loads any
//!    persisted value (or `Default`), inserts it as a Bevy `Resource`, and
//!    appends an entry to [`SettingsRegistry`].
//! 3. Each frame, [`registry::emit_restart_events`] diffs every registered
//!    resource against its [`registry::PreviousSnapshot`]; any change to a
//!    `requires_restart` field writes a [`event::SketchRestart`] message.
//! 4. The user panel (`panel_user`, private) iterates the registry and draws
//!    only `category = User` fields. The dev panel ([`panel_dev`]) opens a
//!    `bevy-inspector-egui` window when [`panel_dev::DevPanelVisible`] is
//!    true, exposing every Reflect-registered resource (including the
//!    sketch settings types, which `register_sketch_settings` registers
//!    automatically).
//! 5. A debounced auto-save system ([`autosave::detect_changes`] +
//!    [`autosave::tick`]) observes resource changes and writes the affected
//!    settings to disk after a quiet window of [`autosave::DEBOUNCE_SECS`]
//!    seconds. Callers can also invoke [`persistence::save::<S>`] directly.

pub mod autosave;
pub mod commands;
pub mod custom_section;
pub mod def;
pub mod event;
pub mod hand_tracking;
pub mod input_capture;
pub mod panel_dev;
pub mod persistence;
pub mod registry;
pub mod trait_def;

mod panel_user;

pub use commands::set_setting;
pub use custom_section::{CustomDockSections, DockSectionFn, RegisterDockSectionExt};
pub use def::{enum_variant_names, NumberRange, SettingDef, SettingKind, SettingsCategory};
pub use event::SketchRestart;
pub use hand_tracking::{HandProviderChoice, HandTrackingSettings};
pub use input_capture::{EguiKeyboardCaptured, EguiPointerCaptured};
pub use panel_dev::DevPanelVisible;
pub use registry::{RegisterSketchSettingsExt, SettingsRegistry};
pub use trait_def::SketchSettings;

use bevy::prelude::*;

/// Plugin that wires the settings subsystem into a Bevy [`App`].
///
/// Registered by [`crate::CorePlugin`]. Sketches register their concrete
/// settings types separately via
/// [`registry::RegisterSketchSettingsExt::register_sketch_settings`].
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SettingsRegistry>()
            .init_resource::<DevPanelVisible>()
            .init_resource::<custom_section::CustomDockSections>()
            .init_resource::<autosave::AutosaveState>()
            .init_resource::<EguiPointerCaptured>()
            .init_resource::<EguiKeyboardCaptured>()
            // The panel's `settings_panel_visible` run condition reads
            // SettingsPanelVisible as a hard param and runs whenever
            // SettingsPlugin is loaded — even in MinimalPlugins test harnesses
            // that don't include OverlayButtonsPlugin. Init it here so parameter
            // validation never panics. Idempotent with OverlayButtonsPlugin's
            // own init.
            .init_resource::<crate::ui::buttons::SettingsPanelVisible>()
            .add_message::<SketchRestart>()
            // Real sketches register their own settings via `LinePlugin`, `FlamePlugin`,
            // etc. The synthetic TestSketchSettings only lives in #[cfg(test)] builds
            // where it backs the integration tests in this crate's tests/ directory.
            .add_systems(
                Update,
                (
                    input_capture::update_egui_input_capture,
                    // The egui_not_capturing_keyboard gate (Shift+D must not
                    // toggle the panel while a panel text field has keyboard
                    // focus) moved to the ActionInput producer in
                    // LifecyclePlugin (PreUpdate), so it is no longer needed
                    // here.
                    panel_dev::handle_dev_panel_toggle,
                    registry::emit_restart_events,
                    autosave::detect_changes,
                    autosave::tick,
                    autosave::flush_on_exit,
                )
                    .chain(),
            );
        // egui-based UI systems are wired below.
        panel_user::add_systems(app);
        panel_dev::add_systems(app);
    }
}
